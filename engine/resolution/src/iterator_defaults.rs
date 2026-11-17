//! Trusted default method templates for `Iterator` and `IntoIterator` (`std.iter`).

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{ScopeId, SymbolId, SymbolKind, Visibility};

#[derive(Default)]
pub(crate) struct IteratorDefaultCache {
    pub(crate) template_symbols: HashSet<SymbolId>,
    pub(crate) template_def_nodes: HashSet<NodeIndex>,
    pub(crate) nested_template_nodes: HashSet<NodeIndex>,
    pub(crate) by_owner_name: HashMap<(TypeIndex, StrId), SymbolId>,
}

pub(crate) fn is_iterator_trait(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    let wk = r.type_pool.well_known;
    canon == wk.iterator || canon == wk.into_iterator
}

/// Bind bootstrap associated type markers (`Item` for `Iterator`, `Iter` for `IntoIterator`)
/// into privileged `impl Iterator` / `impl IntoIterator` scopes so default method signatures
/// and bodies in `std.iter` resolve `Item` and `Iter` directly to their abstract markers.
pub(crate) fn bind_bootstrap_assoc_types(r: &mut Resolver<'_>, scope_id: ScopeId) {
    let Some(owner) = r.scopes[scope_id.0 as usize]
        .assoc_type
        .and_then(|ty| r.type_pool.canonical_type(ty))
    else {
        return;
    };
    if !is_iterator_trait(r, owner) {
        return;
    }
    let assoc_pairs = match &r.type_pool.get(owner).kind {
        TypeKind::Trait { assoc_types, .. } => assoc_types.clone(),
        _ => return,
    };
    let prev_scope = r.current_scope;
    r.current_scope = scope_id;
    for (assoc_name, marker) in assoc_pairs {
        if r.scopes[scope_id.0 as usize]
            .bindings
            .contains_key(&assoc_name)
        {
            continue;
        }
        let sym_id = r.define_symbol(
            assoc_name,
            SymbolKind::Type,
            NodeIndex::NULL,
            Visibility::Private,
        );
        r.symbol_mut(sym_id).type_index = marker;
    }
    r.current_scope = prev_scope;
}

fn compute_is_default_template(r: &Resolver<'_>, sym_id: SymbolId) -> bool {
    let Some(symbol) = r.symbols.get(sym_id.0 as usize) else {
        return false;
    };
    if symbol.kind != SymbolKind::Function
        || symbol.def_node.is_null()
        || !r.options.privileged_nodes.contains(&symbol.def_node)
    {
        return false;
    }
    let Some(assoc) = r
        .scopes
        .get(symbol.scope.0 as usize)
        .and_then(|sc| sc.assoc_type)
        .and_then(|ty| r.type_pool.canonical_type(ty))
    else {
        return false;
    };
    let wk = r.type_pool.well_known;
    if assoc == wk.iterator {
        symbol.name != str_interner::intern("next")
    } else if assoc == wk.into_iterator {
        symbol.name != str_interner::intern("into_iter")
    } else {
        false
    }
}

pub(crate) fn prepare(r: &mut Resolver<'_>, ast: &Ast) {
    let mut cache = IteratorDefaultCache::default();
    let mut stack = Vec::new();
    for sym in &r.symbols {
        if compute_is_default_template(r, sym.id) {
            cache.template_symbols.insert(sym.id);
            cache.template_def_nodes.insert(sym.def_node);
            if let Some(owner) = r.scopes[sym.scope.0 as usize]
                .assoc_type
                .and_then(|ty| r.type_pool.canonical_type(ty))
            {
                cache.by_owner_name.insert((owner, sym.name), sym.id);
            }
            stack.push(sym.def_node);
        }
    }
    while let Some(node) = stack.pop() {
        if node.is_null() {
            continue;
        }
        if ast.node(node).kind == NodeKind::FunctionDef {
            cache.nested_template_nodes.insert(node);
        }
        stack.extend(ast.fixed_children(node));
        stack.extend(ast.multi_children(node));
    }
    r.iterator_defaults = cache;
}

#[inline]
pub(crate) fn is_default_template(r: &Resolver<'_>, sym_id: SymbolId) -> bool {
    r.iterator_defaults.template_symbols.contains(&sym_id)
}

#[inline]
pub(crate) fn is_template_node(r: &Resolver<'_>, node: NodeIndex) -> bool {
    !node.is_null() && r.iterator_defaults.template_def_nodes.contains(&node)
}

#[inline]
pub(crate) fn default_template(
    r: &Resolver<'_>,
    owner: TypeIndex,
    name: StrId,
) -> Option<SymbolId> {
    let canon_owner = r.type_pool.canonical_type(owner)?;
    r.iterator_defaults
        .by_owner_name
        .get(&(canon_owner, name))
        .copied()
}

fn has_inherent_method(
    r: &Resolver<'_>,
    implementor: TypeIndex,
    name: StrId,
    visible_scope: Option<u32>,
) -> bool {
    r.type_pool.methods_of(implementor).iter().any(|slot| {
        slot.name == name && slot.trait_impl.is_none() && slot.visible_scope == visible_scope
    })
}

/// Publish concrete `DefaultMethodPlan` adapters for `IntoIterator` and `Iterator`
/// implementations on demand.
pub(crate) fn complete_records(r: &mut Resolver<'_>, ast: &Ast) {
    let wk = r.type_pool.well_known;
    let into_templates: Vec<SymbolId> = r
        .symbols
        .iter()
        .filter(|sym| {
            r.scopes[sym.scope.0 as usize]
                .assoc_type
                .and_then(|ty| r.type_pool.canonical_type(ty))
                == Some(wk.into_iterator)
                && is_default_template(r, sym.id)
        })
        .map(|sym| sym.id)
        .collect();
    let iter_templates: Vec<SymbolId> = r
        .symbols
        .iter()
        .filter(|sym| {
            r.scopes[sym.scope.0 as usize]
                .assoc_type
                .and_then(|ty| r.type_pool.canonical_type(ty))
                == Some(wk.iterator)
                && is_default_template(r, sym.id)
        })
        .map(|sym| sym.id)
        .collect();
    if into_templates.is_empty() && iter_templates.is_empty() {
        return;
    }

    let mut static_demands: HashSet<(TypeIndex, SymbolId)> = HashSet::new();
    for (&proj_node, &fn_sym) in &r.instance_methods {
        if !is_default_template(r, fn_sym) {
            continue;
        }
        let recv_node = ast.fixed_children(proj_node)[0];
        if let Some(&recv_ty) = r.node_types.get(&recv_node)
            && let Some(canon) = r.type_pool.canonical_type(recv_ty)
        {
            static_demands.insert((canon, fn_sym));
        }
    }
    for plan in &r.default_methods {
        let Some(facts) = &plan.body_facts else {
            continue;
        };
        for (&proj_node, &fn_sym) in &facts.instance_methods {
            if !is_default_template(r, fn_sym) {
                continue;
            }
            let recv_node = ast.fixed_children(proj_node)[0];
            let recv_ty = plan
                .specialized_node_types
                .get(&recv_node)
                .or_else(|| r.node_types.get(&recv_node))
                .copied();
            if let Some(ty) = recv_ty
                && let Some(canon) = r.type_pool.canonical_type(ty)
            {
                static_demands.insert((canon, fn_sym));
            }
        }
    }

    let mut dynamic_member_names: HashSet<StrId> = HashSet::new();
    let mut stack = vec![ast.root];
    while let Some(node) = stack.pop() {
        if node.is_null()
            || r.options.privileged_nodes.contains(&node)
            || ast.node(node).kind == NodeKind::TraitDeriveFn
        {
            continue;
        }
        if ast.node(node).kind == NodeKind::Projection {
            let ch = ast.fixed_children(node);
            if !ch[0].is_null() && !ch[1].is_null() {
                let is_dyn = match r.node_types.get(&ch[0]).copied() {
                    None => !r.node_symbols.contains_key(&node),
                    Some(ty) => r
                        .type_pool
                        .canonical_type(ty)
                        .is_some_and(|c| r.type_pool.as_intrinsic(c) == Some(Intrinsic::Any)),
                };
                if is_dyn {
                    dynamic_member_names.insert(ast.node(ch[1]).str_id);
                }
            }
        }
        for &c in ast.fixed_children(node) {
            if !c.is_null() {
                stack.push(c);
            }
        }
        for &c in ast.multi_children(node) {
            if !c.is_null() {
                stack.push(c);
            }
        }
    }
    for plan in &r.default_methods {
        let def_node = r.symbols[plan.declaration.0 as usize].def_node;
        let mut dstack = vec![def_node];
        while let Some(node) = dstack.pop() {
            if node.is_null() || r.options.privileged_nodes.contains(&node) {
                continue;
            }
            if ast.node(node).kind == NodeKind::Projection {
                let ch = ast.fixed_children(node);
                if !ch[0].is_null() && !ch[1].is_null() {
                    let recv_ty = plan
                        .specialized_node_types
                        .get(&ch[0])
                        .or_else(|| r.node_types.get(&ch[0]))
                        .copied();
                    if recv_ty
                        .and_then(|t| r.type_pool.canonical_type(t))
                        .is_some_and(|c| r.type_pool.as_intrinsic(c) == Some(Intrinsic::Any))
                    {
                        dynamic_member_names.insert(ast.node(ch[1]).str_id);
                    }
                }
            }
            for &c in ast.fixed_children(node) {
                if !c.is_null() {
                    dstack.push(c);
                }
            }
            for &c in ast.multi_children(node) {
                if !c.is_null() {
                    dstack.push(c);
                }
            }
        }
    }

    let records = r.type_pool.trait_impls_snapshot().to_vec();

    for record in records {
        let Some(canon_trait) = r.type_pool.canonical_type(record.trait_type) else {
            continue;
        };
        let templates = if canon_trait == wk.into_iterator {
            &into_templates
        } else if canon_trait == wk.iterator {
            &iter_templates
        } else {
            continue;
        };
        if templates.is_empty() {
            continue;
        }
        let Some(canon_impl) = r.type_pool.canonical_type(record.implementor) else {
            continue;
        };

        for &declaration in templates {
            let name = r.symbols[declaration.0 as usize].name;
            if record.methods.iter().any(|method| method.name == name) {
                continue;
            }
            let needed = static_demands.contains(&(canon_impl, declaration))
                || dynamic_member_names.contains(&name);
            if !needed {
                continue;
            }
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
                    .error(format!("cannot publish iterator default: {error}"))
                    .emit(r.diag_ctx);
                continue;
            }
            if !has_inherent_method(r, record.implementor, name, record.visible_scope) {
                r.type_pool.add_method(record.implementor, slot);
            }
        }
    }
}
