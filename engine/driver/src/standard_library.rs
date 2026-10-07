//! Load trusted standard-library source units into the same compilation graph.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::Diagnostic;
use resolution::ResolveOptions;

use crate::Driver;

const MODULES: &[(&str, &str)] = &[
    ("builtin", include_str!("../../../library/std/builtin.ns")),
    ("ordering", include_str!("../../../library/std/ordering.ns")),
    ("traits", include_str!("../../../library/std/traits.ns")),
    ("io", include_str!("../../../library/std/io.ns")),
    ("math", include_str!("../../../library/std/math.ns")),
    ("string", include_str!("../../../library/std/string.ns")),
    ("process", include_str!("../../../library/std/process.ns")),
    (
        "collections",
        include_str!("../../../library/std/collections.ns"),
    ),
    ("prelude", include_str!("../../../library/std/prelude.ns")),
];

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
}

fn mark_trusted(ast: &Ast, node: NodeIndex, nodes: &mut HashSet<NodeIndex>) {
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
