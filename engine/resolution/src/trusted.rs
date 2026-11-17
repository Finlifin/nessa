//! Lookup of declarations owned by the trusted standard-library package.

use type_pool::TypeIndex;

use crate::{ScopeId, Symbol, SymbolId, SymbolKind, resolver::Resolver};

/// Find `std.<module>.<name>` of `kind` declared by trusted standard-library
/// source. User declarations with the same names are never selected.
fn std_symbol<'r>(
    r: &'r Resolver<'_>,
    module: &str,
    name: &str,
    kind: SymbolKind,
) -> Option<(SymbolId, &'r Symbol)> {
    let std_id = str_interner::intern("std");
    let mod_id = str_interner::intern(module);
    let item_id = str_interner::intern(name);

    // Fast O(1) path via `module_scopes` for normal multi-file `std` compilation.
    for &pkg_node in &r.options.package_roots {
        let pkg_name_node = r
            .scopes
            .iter()
            .find(|sc| sc.node == pkg_node)
            .map(|sc| sc.id)
            .or_else(|| r.imports.module_scopes.get(&pkg_node).copied());
        if let Some(std_scope_id) = pkg_name_node
            && let Some(&mod_sym_id) = r.scopes[std_scope_id.0 as usize].bindings.get(&mod_id)
        {
            let mod_sym = &r.symbols[mod_sym_id.0 as usize];
            if mod_sym.kind == SymbolKind::Module
                && let Some(&mod_scope_id) = r.imports.module_scopes.get(&mod_sym.def_node)
                && let Some(&item_sym_id) = r.scopes[mod_scope_id.0 as usize].bindings.get(&item_id)
            {
                let item_sym = &r.symbols[item_sym_id.0 as usize];
                if item_sym.kind == kind && r.options.privileged_nodes.contains(&item_sym.def_node)
                {
                    return Some((item_sym_id, item_sym));
                }
            }
        }
    }

    // Fallback for standalone unit-test ASTs (`mod std { pub mod ordering { ... } }`).
    let root_std_sym = r.scopes[ScopeId::ROOT.0 as usize]
        .bindings
        .get(&std_id)
        .copied()?;
    let std_def = r.symbols[root_std_sym.0 as usize].def_node;
    if !r.options.package_roots.contains(&std_def) {
        return None;
    }
    let std_scope = r
        .imports
        .module_scopes
        .get(&std_def)
        .copied()
        .or_else(|| r.scopes.iter().find(|s| s.node == std_def).map(|s| s.id))?;
    let mod_sym_id = *r.scopes[std_scope.0 as usize].bindings.get(&mod_id)?;
    let mod_def = r.symbols[mod_sym_id.0 as usize].def_node;
    let mod_scope = r
        .imports
        .module_scopes
        .get(&mod_def)
        .copied()
        .or_else(|| r.scopes.iter().find(|s| s.node == mod_def).map(|s| s.id))?;
    let item_sym_id = *r.scopes[mod_scope.0 as usize].bindings.get(&item_id)?;
    let item_sym = &r.symbols[item_sym_id.0 as usize];
    (item_sym.kind == kind && r.options.privileged_nodes.contains(&item_sym.def_node))
        .then_some((item_sym_id, item_sym))
}

pub(crate) fn std_type<'r>(
    r: &'r Resolver<'_>,
    module: &str,
    name: &str,
) -> Option<(TypeIndex, &'r Symbol)> {
    let (_, symbol) = std_symbol(r, module, name, SymbolKind::Type)?;
    let ty = r.type_pool.canonical_type(symbol.type_index)?;
    Some((ty, symbol))
}

pub(crate) fn std_function(r: &Resolver<'_>, module: &str, name: &str) -> Option<SymbolId> {
    std_symbol(r, module, name, SymbolKind::Function).map(|(id, _)| id)
}
