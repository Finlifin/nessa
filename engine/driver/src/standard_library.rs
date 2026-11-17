//! Load trusted standard-library source units into the same compilation graph.

use std::collections::HashSet;
use std::sync::OnceLock;

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::Diagnostic;
use resolution::ResolveOptions;
use sha2::{Digest, Sha256};

use crate::Driver;

const MODULES: &[(&str, &str)] = &[
    ("builtin", include_str!("../../../library/std/builtin.ns")),
    ("ordering", include_str!("../../../library/std/ordering.ns")),
    ("traits", include_str!("../../../library/std/traits.ns")),
    ("conv", include_str!("../../../library/std/conv.ns")),
    ("iter", include_str!("../../../library/std/iter.ns")),
    ("stream", include_str!("../../../library/std/stream.ns")),
    ("io", include_str!("../../../library/std/io.ns")),
    ("fs", include_str!("../../../library/std/fs.ns")),
    ("path", include_str!("../../../library/std/path.ns")),
    ("env", include_str!("../../../library/std/env.ns")),
    ("time", include_str!("../../../library/std/time.ns")),
    ("math", include_str!("../../../library/std/math.ns")),
    ("string", include_str!("../../../library/std/string.ns")),
    ("process", include_str!("../../../library/std/process.ns")),
    (
        "collections",
        include_str!("../../../library/std/collections.ns"),
    ),
    (
        "coroutine",
        include_str!("../../../library/std/coroutine.ns"),
    ),
    ("runtime", include_str!("../../../library/std/runtime.ns")),
    ("json", include_str!("../../../library/std/json.ns")),
    ("range", include_str!("../../../library/std/range.ns")),
    ("scalar", include_str!("../../../library/std/scalar.ns")),
    ("fmt", include_str!("../../../library/std/fmt.ns")),
    ("pattern", include_str!("../../../library/std/pattern.ns")),
    ("bytes", include_str!("../../../library/std/bytes.ns")),
    ("rand", include_str!("../../../library/std/rand.ns")),
    ("task", include_str!("../../../library/std/task.ns")),
    ("sync", include_str!("../../../library/std/sync.ns")),
    ("net", include_str!("../../../library/std/net.ns")),
    ("wasm", include_str!("../../../library/std/wasm.ns")),
    ("ffi", include_str!("../../../library/std/ffi.ns")),
    ("prelude", include_str!("../../../library/std/prelude.ns")),
];

pub(crate) fn std_library_sha256() -> [u8; 32] {
    static STD_DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    *STD_DIGEST.get_or_init(|| {
        let mut hasher = Sha256::new();
        hasher.update(b"nessa.std.sources.v1\0");
        let manifest = include_str!("../../../library/std/package.toml");
        hasher.update((manifest.len() as u64).to_be_bytes());
        hasher.update(manifest.as_bytes());
        let root_mod = include_str!("../../../library/std/mod.ns");
        hasher.update((root_mod.len() as u64).to_be_bytes());
        hasher.update(root_mod.as_bytes());
        hasher.update((MODULES.len() as u64).to_be_bytes());
        for &(name, source) in MODULES {
            hasher.update((name.len() as u64).to_be_bytes());
            hasher.update(name.as_bytes());
            hasher.update((source.len() as u64).to_be_bytes());
            hasher.update(source.as_bytes());
        }
        hasher.finalize().into()
    })
}

impl Driver {
    pub(crate) fn load_standard_library(
        &self,
        mut ast: Ast,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(Ast, ResolveOptions), Vec<Diagnostic>> {
        let user_items = ast.multi_children(ast.root).to_vec();
        let mut options = ResolveOptions {
            expose_root_builtins: false,
            expose_root_types: false,
            ..ResolveOptions::default()
        };
        let (source, warnings) =
            self.parse_source_named(include_str!("../../../library/std/mod.ns"), "std/mod.ns")?;
        diagnostics.extend(warnings);
        let package_body = self.append_source_units(&mut ast, vec![source])?[0];
        mark_trusted(&ast, package_body, &mut options.privileged_nodes);
        let members = ast.multi_children(package_body).to_vec();
        let mut sources = Vec::with_capacity(MODULES.len());
        for &(name, source) in MODULES {
            let (module, warnings) = self.parse_source_named(source, &format!("std/{name}.ns"))?;
            diagnostics.extend(warnings);
            sources.push(module);
        }
        let bodies = self.append_source_units(&mut ast, sources)?;
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
                let context = diagnostic::DiagnosticContext::new(&self.source_map);
                context
                    .error(format!("std/mod.ns must declare module `{name}`"))
                    .with_primary_span(ast.node(package_body).span)
                    .emit(&context);
                return Err(context.diagnostics().to_vec());
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
            let context = diagnostic::DiagnosticContext::new(&self.source_map);
            context
                .error(format!(
                    "invalid standard-library package identity: {message}"
                ))
                .with_primary_span(span)
                .emit(&context);
            context.diagnostics().to_vec()
        })?;
        options.package_identities.insert(package, identity);
        let package = ast
            .builder(NodeKind::PubDef, span)
            .add_child(package)
            .build();
        let (prelude, warnings) =
            self.parse_source_named("use std.prelude.*", "implicit-prelude")?;
        diagnostics.extend(warnings);
        let prelude = self.append_source_units(&mut ast, vec![prelude])?[0];
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
    pub(crate) fn attach_bundled_script_packages(
        &self,
        ast: &mut Ast,
        options: &mut ResolveOptions,
        std_root: Option<NodeIndex>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(), Vec<Diagnostic>> {
        let mut bundled_mods = Vec::new();
        for pkg in pkg_manager::bundled_packages() {
            let manifest = pkg.manifest.manifest();
            let (pkg_ast, warnings) = self.package_ast(pkg)?;
            diagnostics.extend(warnings);
            let body = self.append_source_units(ast, vec![pkg_ast])?[0];

            let (prelude, warnings) =
                self.parse_source_named("use std.prelude.*", "implicit-bundled-prelude")?;
            diagnostics.extend(warnings);
            let prelude_node = self.append_source_units(ast, vec![prelude])?[0];
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
                    let context = diagnostic::DiagnosticContext::new(&self.source_map);
                    context
                        .error(format!("invalid bundled package identity: {message}"))
                        .with_primary_span(span)
                        .emit(&context);
                    context.diagnostics().to_vec()
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

pub(crate) fn mark_trusted(ast: &Ast, node: NodeIndex, nodes: &mut HashSet<NodeIndex>) {
    if node.is_null() || !nodes.insert(node) {
        return;
    }
    for &child in ast
        .fixed_children(node)
        .iter()
        .chain(ast.multi_children(node))
    {
        mark_trusted(ast, child, nodes);
    }
}

pub(crate) fn mark_ffi_permissions(
    ast: &Ast,
    node: NodeIndex,
    ffi: &pkg_manager::FfiConfig,
    options: &mut ResolveOptions,
) {
    if !ffi.allow {
        return;
    }
    let allowlist = (!ffi.libraries.is_empty())
        .then(|| ffi.libraries.iter().cloned().collect::<HashSet<String>>());
    mark_ffi_nodes_recursive(ast, node, allowlist.as_ref(), options);
}

fn mark_ffi_nodes_recursive(
    ast: &Ast,
    node: NodeIndex,
    allowlist: Option<&HashSet<String>>,
    options: &mut ResolveOptions,
) {
    if node.is_null() || !options.ffi_nodes.insert(node) {
        return;
    }
    if let Some(allowed) = allowlist {
        options.ffi_library_allowlists.insert(node, allowed.clone());
    }
    for &child in ast
        .fixed_children(node)
        .iter()
        .chain(ast.multi_children(node))
    {
        mark_ffi_nodes_recursive(ast, child, allowlist, options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use type_pool::{Intrinsic, TypeKind};

    #[test]
    fn standard_library_signatures_come_from_source_annotations() {
        let driver = Driver::new();
        let (ast, mut diagnostics) = driver.parse_source("fn main() { 42 }").unwrap();
        let (ast, options) = driver.load_standard_library(ast, &mut diagnostics).unwrap();
        let context = diagnostic::DiagnosticContext::new(&driver.source_map);
        let resolved = resolution::resolve_with_options(ast, &context, options);
        assert!(!context.has_errors(), "{:?}", context.diagnostics());
        let root = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        for (name, expected) in [("sin", Intrinsic::F64), ("to_i64", Intrinsic::Any)] {
            let symbol = &resolved.symbols[root.bindings[&str_interner::intern(name)].0 as usize];
            assert_ne!(
                symbol.type_index,
                type_pool::TypeIndex::INVALID,
                "{symbol:?}, definition {:?}",
                resolved.ast.node(symbol.def_node)
            );
            assert!(
                matches!(&resolved.type_pool.get(symbol.type_index).kind,
                TypeKind::Function { params, .. } if params == &[expected.type_index()]),
                "{symbol:?}, definition {:?}",
                resolved.ast.node(symbol.def_node)
            );
        }
        assert!(
            resolved.scopes[resolution::ScopeId::ROOT.0 as usize]
                .bindings
                .is_empty()
        );
    }
}
