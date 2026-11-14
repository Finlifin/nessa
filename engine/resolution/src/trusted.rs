//! Lookup of declarations owned by the trusted standard-library package.

use type_pool::TypeIndex;

use crate::{Symbol, SymbolId, SymbolKind, resolver::Resolver};

/// Find `std.<module>.<name>` of `kind` declared by trusted standard-library
/// source. User declarations with the same names are never selected.
fn std_symbol<'r>(
    r: &'r Resolver<'_>,
    module: &str,
    name: &str,
    kind: SymbolKind,
) -> Option<(SymbolId, &'r Symbol)> {
    r.symbols.iter().enumerate().find_map(|(index, symbol)| {
        if symbol.kind != kind
            || str_interner::get(symbol.name) != name
            || !r.options.privileged_nodes.contains(&symbol.def_node)
        {
            return None;
        }
        let module_scope = r.scopes.get(symbol.scope.0 as usize)?;
        let declaration = r.symbols.iter().find(|candidate| {
            candidate.kind == SymbolKind::Module
                && candidate.def_node == module_scope.node
                && str_interner::get(candidate.name) == module
        })?;
        let package = r.scopes.get(declaration.scope.0 as usize)?;
        let trusted_package = r.symbols.iter().any(|candidate| {
            candidate.kind == SymbolKind::Module
                && candidate.def_node == package.node
                && str_interner::get(candidate.name) == "std"
                && r.options.package_roots.contains(&candidate.def_node)
        });
        trusted_package.then_some((SymbolId(index as u32), symbol))
    })
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
