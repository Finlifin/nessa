//! Ordering is an ordinary nominal enum declared by the trusted standard library.

use type_pool::{TypeIndex, TypeKind};

use crate::{SymbolKind, resolver::Resolver};

pub(crate) fn type_index(r: &Resolver<'_>) -> Option<TypeIndex> {
    r.symbols.iter().find_map(|symbol| {
        if symbol.kind != SymbolKind::Type
            || str_interner::get(symbol.name) != "Ordering"
            || !r.options.privileged_nodes.contains(&symbol.def_node)
        {
            return None;
        }
        let module = r.scopes.get(symbol.scope.0 as usize)?;
        let declaration = r.symbols.iter().find(|candidate| {
            candidate.kind == SymbolKind::Module
                && candidate.def_node == module.node
                && str_interner::get(candidate.name) == "ordering"
        })?;
        let package = r.scopes.get(declaration.scope.0 as usize)?;
        let trusted_package = r.symbols.iter().any(|candidate| {
            candidate.kind == SymbolKind::Module
                && candidate.def_node == package.node
                && str_interner::get(candidate.name) == "std"
                && r.options.package_roots.contains(&candidate.def_node)
        });
        let ty = r.type_pool.canonical_type(symbol.type_index)?;
        (trusted_package && matches!(r.type_pool.get(ty).kind, TypeKind::Enum { .. })).then_some(ty)
    })
}

pub(crate) fn prepare(r: &mut Resolver<'_>) {
    if type_index(r).is_none() {
        return;
    }
    let known = r.type_pool.well_known;
    for (child, parent) in [(known.ord, known.eq), (known.partial_ord, known.partial_eq)] {
        if let TypeKind::Trait { parents, .. } = &mut r.type_pool.get_mut(child).kind {
            *parents = vec![parent];
        }
    }
}

pub(crate) fn default_template(
    r: &Resolver<'_>,
    name: str_interner::StrId,
) -> Option<crate::SymbolId> {
    if type_index(r).is_none()
        || !["lt", "gt", "lte", "gte"]
            .into_iter()
            .any(|candidate| name == str_interner::intern(candidate))
    {
        return None;
    }
    r.symbols
        .iter()
        .find(|symbol| {
            symbol.name == name
                && symbol.kind == SymbolKind::Function
                && r.options.privileged_nodes.contains(&symbol.def_node)
                && r.scopes[symbol.scope.0 as usize].assoc_type == Some(r.type_pool.well_known.ord)
        })
        .map(|symbol| symbol.id)
}

/// Publish defaults after source and derived records have all been registered.
pub(crate) fn complete_records(r: &mut Resolver<'_>, ast: &ast::Ast) {
    if type_index(r).is_none() {
        return;
    }
    let records = r.type_pool.trait_impls_snapshot().to_vec();
    for record in records {
        if record.trait_type != r.type_pool.well_known.ord {
            continue;
        }
        for name in ["lt", "gt", "lte", "gte"] {
            let name = str_interner::intern(name);
            if record.methods.iter().any(|method| method.name == name) {
                continue;
            }
            let Some(declaration) = default_template(r, name) else {
                continue;
            };
            let provider = r.symbols[declaration.0 as usize].scope.0;
            let function = match crate::default_methods::register(
                r,
                ast,
                declaration,
                record.implementor,
                record.trait_type,
                record.visible_scope,
                provider,
            ) {
                Ok(function) => function,
                Err(error) => {
                    r.diag_ctx
                        .error(error)
                        .with_primary_span(
                            ast.node(r.symbols[declaration.0 as usize].def_node).span,
                        )
                        .emit(r.diag_ctx);
                    continue;
                }
            };
            let slot = type_pool::MethodSlot {
                access: type_pool::MethodAccess::Public,
                name,
                func_id: function.0,
                trait_impl: Some(record.trait_type),
                visible_scope: record.visible_scope,
            };
            if let Err(error) = r.type_pool.add_trait_method(
                record.implementor,
                record.trait_type,
                record.visible_scope,
                slot.clone(),
            ) {
                r.diag_ctx
                    .error(format!("cannot publish ordering default: {error}"))
                    .emit(r.diag_ctx);
                continue;
            }
            r.type_pool.add_method(record.implementor, slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use ast::{NodeIndex, NodeKind};
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
    use type_pool::{Intrinsic, TypeKind};

    #[test]
    fn ordering_contracts_use_only_the_trusted_nominal_source_enum() {
        let source = "mod std{pub mod ordering{pub enum Ordering{pub less,pub equal,pub greater};derive Eq,PartialEq,Ord,PartialOrd for Ordering;typealias OrderView=.Ord'builtin;impl OrderView{pub fn lt(self,other:Self)->bool{true};pub fn gt(self,other:Self)->bool{false};pub fn lte(self,other:Self)->bool{true};pub fn gte(self,other:Self)->bool{false}}}}";
        for trusted in [false, true] {
            let (tokens, errors) = lexer::tokenize(source);
            assert!(errors.is_empty(), "{errors:?}");
            let map = SourceMap::new(FilePathMapping::empty());
            let file = map.new_source_file(
                FileName::Custom("ordering-contract.ns".into()),
                source.into(),
            );
            let diagnostics = DiagnosticContext::new(&map);
            let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
            let mut options = crate::ResolveOptions::for_builtin_package();
            if trusted {
                options.privileged_nodes = (1..ast.nodes.len())
                    .map(|index| NodeIndex(index as u32))
                    .collect();
                options.package_roots = (1..ast.nodes.len())
                    .map(|index| NodeIndex(index as u32))
                    .filter(|&node| {
                        ast.node(node).kind == NodeKind::ModuleDef
                            && str_interner::get(ast.node(ast.fixed_children(node)[0]).str_id)
                                == "std"
                    })
                    .collect();
            }
            let resolved = crate::resolve_with_options(ast, &diagnostics, options);
            let pool = &resolved.type_pool;
            let known = pool.well_known;
            let schema = pool.trait_schema(known.ord).unwrap();
            if !trusted {
                assert!(
                    diagnostics.has_errors(),
                    "untrusted name must not establish the ordering protocol"
                );
                assert_eq!(schema.slots.len(), 1);
                assert!(schema.slots[0].signature.is_none());
                continue;
            }
            assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
            assert_eq!(
                schema
                    .slots
                    .iter()
                    .map(|key| str_interner::get(key.name))
                    .collect::<Vec<_>>(),
                ["eq", "cmp", "lt", "gt", "lte", "gte"]
            );
            let signature = schema.slots[1].signature.as_ref().unwrap();
            let TypeKind::Function { ret: ordering, .. } = pool.get(signature.declaration).kind
            else {
                panic!("cmp signature");
            };
            assert!(matches!(pool.get(ordering).kind, TypeKind::Enum { .. }));
            for key in &schema.slots[2..] {
                let TypeKind::Function { ret, .. } =
                    pool.get(key.signature.as_ref().unwrap().declaration).kind
                else {
                    panic!("helper signature");
                };
                assert_eq!(ret, Intrinsic::Bool.type_index());
            }
            let partial = pool.trait_schema(known.partial_ord).unwrap();
            let TypeKind::Function { ret, .. } = pool
                .get(partial.slots[1].signature.as_ref().unwrap().declaration)
                .kind
            else {
                panic!("partial_cmp signature");
            };
            assert!(
                matches!(pool.get(ret).kind, TypeKind::Optional { inner } if inner == ordering)
            );
            assert_eq!(resolved.default_methods.len(), 4);
        }
    }
}
