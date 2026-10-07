//! Phase 3d: Trait & Method Resolution
//!
//! Collects [`TraitInfo`] entries (required / derived methods) from all
//! trait symbols registered by Phase 3a.  Also records impl-trait
//! relationships discovered from `ImplTraitDef` / `ExtendTraitDef` nodes.

use ast::{Ast, NodeKind};
use type_pool::{MethodSlot, TraitImplRecord, TypeKind, VTable};

use crate::resolver::Resolver;
use crate::{SymbolKind, TraitInfo};

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Collect [`TraitInfo`] entries and record impl-trait relationships.
/// Parent identities are needed by body typing before implementations exist.
pub(crate) fn prepare_trait_parents(r: &mut Resolver, ast: &Ast) {
    for symbol in &r.symbols {
        if symbol.kind != SymbolKind::Trait || symbol.def_node.is_null() {
            continue;
        }
        let parent_node = ast.fixed_children(symbol.def_node)[1];
        if parent_node.is_null() {
            continue;
        }
        let parents = ast
            .multi_children(parent_node)
            .iter()
            .filter_map(|node| {
                let parent = r.symbols[r.node_symbols.get(node)?.0 as usize].type_index;
                let parent = r.type_pool.canonical_type(parent)?;
                matches!(r.type_pool.get(parent).kind, TypeKind::Trait { .. }).then_some(parent)
            })
            .collect();
        if let TypeKind::Trait {
            parents: target, ..
        } = &mut r.type_pool.get_mut(symbol.type_index).kind
        {
            *target = parents;
        }
    }
}

pub(crate) fn resolve_traits(r: &mut Resolver, ast: &Ast) {
    // ── Collect trait method signatures ────────────────────────────
    let trait_defs: Vec<_> = r
        .scopes
        .iter()
        .flat_map(|scope| scope.bindings.values())
        .filter_map(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Trait {
                Some((sym.name, sym.type_index, sym.def_node))
            } else {
                None
            }
        })
        .collect();

    for (name, type_index, def_node) in trait_defs {
        // Skip pre-registered well-known traits (they have NULL def_node).
        if def_node.is_null() {
            continue;
        }

        // TraitDef: [0] name  [1] parents(ListOf/NULL)  multi: members
        let parent_indices = match &r.type_pool.get(type_index).kind {
            TypeKind::Trait { parents, .. } => parents.clone(),
            _ => Vec::new(),
        };

        let mut required = Vec::new();
        let mut derived = Vec::new();

        for &member in ast.multi_children(def_node) {
            let member_kind = ast.node(member).kind;
            let member_children = ast.fixed_children(member);
            match member_kind {
                NodeKind::TraitDefFn => {
                    // Required method — no default body.
                    let fn_name = ast.node(member_children[0]).str_id;
                    required.push(fn_name);
                }
                NodeKind::TraitDeriveFn => {
                    // Derived method — has a default body.
                    let fn_name = ast.node(member_children[0]).str_id;
                    derived.push(fn_name);
                }
                _ => {}
            }
        }

        r.traits.push(TraitInfo {
            name,
            type_index,
            required_methods: required,
            derived_methods: derived,
            parent_traits: parent_indices,
        });
    }

    // ── Record impl-trait relationships ────────────────────────────
    // Walk all scopes looking for ImplTraitDef / ExtendTraitDef nodes.
    let impl_trait_nodes: Vec<_> = r
        .scopes
        .iter()
        .filter(|scope| {
            let node = scope.node;
            if node.is_null() {
                return false;
            }
            let kind = ast.node(node).kind;
            kind == NodeKind::ImplTraitDef || kind == NodeKind::ExtendTraitDef
        })
        .map(|scope| scope.node)
        .collect();

    let mut parent_checks = Vec::new();
    let mut implementations = std::collections::HashSet::new();
    for node_idx in impl_trait_nodes {
        // ImplTraitDef / ExtendTraitDef: [0] trait_expr  [1] type_expr  multi: members
        let children = ast.fixed_children(node_idx);
        let trait_node = children[0];
        let type_node = children[1];

        // Look up the trait's type index via its resolved symbol.
        let trait_ti = r.node_symbols.get(&trait_node).and_then(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            if let Some(ty) = r.type_pool.canonical_type(sym.type_index)
                && matches!(r.type_pool.get(ty).kind, TypeKind::Trait { .. })
            {
                Some(ty)
            } else {
                None
            }
        });

        // Look up the target type's type index.
        let target_ti = r.node_symbols.get(&type_node).and_then(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            r.type_pool.canonical_type(sym.type_index)
        });

        let (Some(trait_type), Some(implementor)) = (trait_ti, target_ti) else {
            continue;
        };
        let visible_scope = r
            .imports
            .extension_scopes
            .get(&r.imports.module_scopes[&node_idx])
            .map(|scope| scope.0);
        if !implementations.insert((implementor, trait_type, visible_scope)) {
            r.diag_ctx
                .error("duplicate trait implementation in the same scope".into())
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
            continue;
        }

        // Collect method implementations from the impl block's members.
        let mut methods = Vec::new();
        let mut provided_names: Vec<str_interner::StrId> = Vec::new();
        for &member in ast.multi_children(node_idx) {
            let member = if matches!(
                ast.node(member).kind,
                NodeKind::PubDef | NodeKind::PrivateDef
            ) {
                ast.fixed_children(member)[0]
            } else {
                member
            };
            if ast.node(member).kind == NodeKind::FunctionDef {
                let fn_children = ast.fixed_children(member);
                let fn_name = ast.node(fn_children[0]).str_id;

                let Some(&method_symbol) = r.node_symbols.get(&fn_children[0]) else {
                    r.diag_ctx
                        .error("method declaration has no resolved function symbol".into())
                        .with_primary_span(ast.node(member).span)
                        .emit(r.diag_ctx);
                    continue;
                };
                let func_id = method_symbol.0;
                let access = crate::access::method_access(r, method_symbol);

                methods.push(MethodSlot {
                    name: fn_name,
                    func_id,
                    access,
                    trait_impl: Some(trait_type),
                    visible_scope,
                });

                r.type_pool.add_method(
                    implementor,
                    MethodSlot {
                        name: fn_name,
                        func_id,
                        access,
                        trait_impl: Some(trait_type),
                        visible_scope,
                    },
                );

                provided_names.push(fn_name);
            }
        }

        // ── Validate required methods & inject defaults ───────────
        if let Some(trait_info) = r
            .traits
            .iter()
            .find(|ti| ti.type_index == trait_type)
            .cloned()
        {
            let required = trait_info.required_methods.clone();
            let derived = trait_info.derived_methods.clone();

            // Check that all required methods are provided.
            for &req_name in &required {
                if !provided_names.contains(&req_name) {
                    let span = ast.node(node_idx).span;
                    let trait_name = r
                        .type_pool
                        .get(trait_type)
                        .kind
                        .trait_name()
                        .unwrap_or(req_name);
                    let mut diag = diagnostic::Diagnostic::error(format!(
                        "impl of trait `{}` is missing required method `{}`",
                        str_interner::get(trait_name),
                        str_interner::get(req_name),
                    ));
                    diag.primary_span = Some(span);
                    r.diagnostics.push(diag);
                }
            }

            // Inject default implementations for derived methods not overridden.
            // Find trait scope to look up the default method's func_id.
            let trait_def_node = r
                .symbols
                .iter()
                .find(|s| s.kind == SymbolKind::Trait && s.type_index == trait_type)
                .map(|s| s.def_node);

            if let Some(trait_node) = trait_def_node
                && !trait_node.is_null()
            {
                // Find the scope for this trait definition.
                let trait_scope = r.scopes.iter().find(|s| s.node == trait_node);

                if let Some(scope) = trait_scope {
                    let bindings = scope.bindings.clone();
                    for &derived_name in &derived {
                        if !provided_names.contains(&derived_name) {
                            // Use the trait's default function symbol.
                            if let Some(&sym_id) = bindings.get(&derived_name) {
                                let provider_scope = r.node_scopes[&node_idx].0;
                                let function = match crate::default_methods::register(
                                    r,
                                    ast,
                                    sym_id,
                                    implementor,
                                    trait_type,
                                    visible_scope,
                                    provider_scope,
                                ) {
                                    Ok(function) => function,
                                    Err(message) => {
                                        r.diag_ctx
                                            .error(message)
                                            .with_primary_span(ast.node(node_idx).span)
                                            .emit(r.diag_ctx);
                                        continue;
                                    }
                                };
                                let default_func_id = function.0;
                                let slot = MethodSlot {
                                    name: derived_name,
                                    func_id: default_func_id,
                                    access: crate::access::method_access(r, sym_id),
                                    trait_impl: Some(trait_type),
                                    visible_scope,
                                };
                                methods.push(slot.clone());
                                r.type_pool.add_method(implementor, slot);
                            }
                        }
                    }
                }
            }

            // All declarations are collected before parent checks, so forward
            // implementations obey the same rules as earlier declarations.
            for &parent in &trait_info.parent_traits {
                parent_checks.push((node_idx, implementor, trait_type, parent, visible_scope));
            }
        }

        // Record the trait implementation.
        let record = TraitImplRecord {
            trait_type,
            implementor,
            methods,
            visible_scope,
        };
        if crate::associated_types::has_declarations(r, trait_type) {
            if let Err(error) = r.type_pool.complete_trait_impl(record) {
                r.diag_ctx
                    .error(error.to_string())
                    .with_primary_span(ast.node(node_idx).span)
                    .emit(r.diag_ctx);
            }
        } else {
            r.type_pool.add_trait_impl(record);
        }

        if crate::ordering::type_index(r).is_some()
            && [
                r.type_pool.well_known.ord,
                r.type_pool.well_known.partial_ord,
            ]
            .contains(&trait_type)
        {
            let parent = if trait_type == r.type_pool.well_known.ord {
                r.type_pool.well_known.eq
            } else {
                r.type_pool.well_known.partial_eq
            };
            parent_checks.push((node_idx, implementor, trait_type, parent, visible_scope));
        }
    }

    crate::associated_types::install_all(r, ast);

    // ── Register methods from plain impl blocks (no trait) ─────────
    let impl_nodes: Vec<_> = r
        .scopes
        .iter()
        .filter(|scope| {
            let node = scope.node;
            if node.is_null() {
                return false;
            }
            let kind = ast.node(node).kind;
            matches!(
                kind,
                NodeKind::ImplDef | NodeKind::ExtendDef | NodeKind::StructDef | NodeKind::EnumDef
            )
        })
        .map(|scope| scope.node)
        .collect();

    for node_idx in impl_nodes {
        // ImplDef/ExtendDef: [0] type_expr  multi: members
        let children = ast.fixed_children(node_idx);
        let type_node = children[0];

        let is_extend = ast.node(node_idx).kind == NodeKind::ExtendDef;
        let visible_scope = if is_extend {
            r.scopes
                .iter()
                .find(|s| s.node == node_idx)
                .and_then(|s| s.parent)
                .map(|sid| sid.0)
        } else {
            None
        };

        let target_ti = r.node_symbols.get(&type_node).and_then(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            r.type_pool.canonical_type(sym.type_index)
        });

        let Some(implementor) = target_ti else {
            continue;
        };

        for &member in ast.multi_children(node_idx) {
            let member = if matches!(
                ast.node(member).kind,
                NodeKind::PubDef | NodeKind::PrivateDef
            ) {
                ast.fixed_children(member)[0]
            } else {
                member
            };
            if ast.node(member).kind == NodeKind::FunctionDef {
                let fn_children = ast.fixed_children(member);
                let fn_name = ast.node(fn_children[0]).str_id;

                let Some(&method_symbol) = r.node_symbols.get(&fn_children[0]) else {
                    r.diag_ctx
                        .error("method declaration has no resolved function symbol".into())
                        .with_primary_span(ast.node(member).span)
                        .emit(r.diag_ctx);
                    continue;
                };
                let func_id = method_symbol.0;
                let access = crate::access::method_access(r, method_symbol);

                r.type_pool.add_method(
                    implementor,
                    MethodSlot {
                        name: fn_name,
                        func_id,
                        access,
                        trait_impl: None,
                        visible_scope,
                    },
                );
            }
        }
    }

    crate::comparison_derivation::register(r, ast);
    crate::ordering::complete_records(r, ast);

    for (node, implementor, trait_type, parent, visible_scope) in parent_checks {
        let available = match visible_scope {
            Some(scope) => r
                .type_pool
                .find_trait_impl_scoped(implementor, parent, scope)
                .map(|record| record.is_some()),
            None => Ok(r.type_pool.has_trait_impl(implementor, parent)),
        };
        match available {
            Ok(true) => {}
            result => {
                let message = match result {
                    Err(error) => format!("ambiguous parent trait implementation: {error}"),
                    _ => format!(
                        "impl of trait `{}` requires a visible parent trait `{}` implementation",
                        r.type_pool.display_name(trait_type).unwrap_or_default(),
                        r.type_pool.display_name(parent).unwrap_or_default()
                    ),
                };
                r.diag_ctx
                    .error(message)
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
            }
        }
    }

    // ── Build vtables for all recorded trait impls ─────────────────
    crate::trait_schemas::register(r, ast);
    build_vtables(r);
    crate::trait_signatures::validate(r, ast);
    crate::display_derivation::validate(r, ast);
}

// ---------------------------------------------------------------------------
// VTable construction
// ---------------------------------------------------------------------------

/// Build vtables for all registered trait impls.
///
/// For each `TraitImplRecord`, we create a `VTable` whose entries are ordered
/// by the canonical method order of the trait (required methods first, then
/// derived methods, in declaration order).  Parent trait methods are prepended
/// so that a sub-trait vtable is a superset of its parent trait vtable(s).
fn build_vtables(r: &mut Resolver) {
    let records = r.type_pool.trait_impls_snapshot().to_vec();
    for record in records {
        let Some(schema) = r.type_pool.trait_schema(record.trait_type) else {
            continue;
        };
        let order: Vec<_> = schema
            .slots
            .iter()
            .map(|slot| (slot.trait_owner, slot.name))
            .collect();
        let mut entries = Vec::with_capacity(order.len());
        let mut complete = true;
        for (owner, name) in order {
            let own = record.methods.iter().find(|method| method.name == name);
            let inherited = if own.is_none() {
                match record.visible_scope {
                    Some(scope) => match r.type_pool.find_trait_method_scoped(
                        record.implementor,
                        owner,
                        name,
                        scope,
                    ) {
                        Ok(method) => method,
                        Err(error) => {
                            complete = false;
                            r.diag_ctx
                                .error(format!("cannot resolve inherited trait method: {error}"))
                                .emit(r.diag_ctx);
                            continue;
                        }
                    },
                    None => r
                        .type_pool
                        .find_trait_method(record.implementor, owner, name),
                }
            } else {
                None
            };
            if let Some(method) = own.or(inherited) {
                entries.push(method.func_id);
            } else {
                complete = false;
                r.diag_ctx
                    .error(format!(
                        "trait vtable is missing checked method `{}`",
                        str_interner::get(name)
                    ))
                    .emit(r.diag_ctx);
            }
        }
        if complete {
            r.type_pool.add_vtable(VTable {
                trait_type: record.trait_type,
                implementor: record.implementor,
                entries,
                visible_scope: record.visible_scope,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Derive method generation
// ---------------------------------------------------------------------------

/// Sentinel func_id indicating a compiler-derived method.
/// Re-exported from type_pool for use by the interpreter.
pub const DERIVE_FUNC_ID: u32 = type_pool::DERIVE_FUNC_ID;

pub(crate) fn generate_derive_methods(
    r: &Resolver,
    _implementor: type_pool::TypeIndex,
    trait_type: type_pool::TypeIndex,
) -> Vec<MethodSlot> {
    let wk = &r.type_pool.well_known;
    let mut methods = Vec::new();

    if trait_type == wk.eq || trait_type == wk.partial_eq {
        methods.push(MethodSlot {
            name: str_interner::intern("eq"),
            func_id: DERIVE_FUNC_ID,
            access: type_pool::MethodAccess::Public,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    } else if trait_type == wk.ord || trait_type == wk.partial_ord {
        methods.push(MethodSlot {
            name: str_interner::intern("cmp"),
            func_id: DERIVE_FUNC_ID,
            access: type_pool::MethodAccess::Public,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    } else if trait_type == wk.hash {
        methods.push(MethodSlot {
            name: str_interner::intern("hash"),
            func_id: DERIVE_FUNC_ID,
            access: type_pool::MethodAccess::Public,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    } else if trait_type == wk.display {
        methods.push(MethodSlot {
            name: str_interner::intern("to_string"),
            func_id: DERIVE_FUNC_ID,
            access: type_pool::MethodAccess::Public,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    }

    methods
}
