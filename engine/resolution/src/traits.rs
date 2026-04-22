//! Phase 3d: Trait & Method Resolution
//!
//! Collects [`TraitInfo`] entries (required / derived methods) from all
//! trait symbols registered by Phase 3a.  Also records impl-trait
//! relationships discovered from `ImplTraitDef` / `ExtendTraitDef` nodes.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{MethodSlot, TraitImplRecord, TypeKind, VTable};

use crate::resolver::Resolver;
use crate::{SymbolKind, TraitInfo};

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Collect [`TraitInfo`] entries and record impl-trait relationships.
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
        let children = ast.fixed_children(def_node);
        let parents_node = children[1];

        // Resolve parent traits.
        let mut parent_indices = Vec::new();
        if !parents_node.is_null() {
            for &parent_expr in ast.multi_children(parents_node) {
                if let Some(&sym_id) = r.node_symbols.get(&parent_expr) {
                    let psym = &r.symbols[sym_id.0 as usize];
                    if psym.kind == SymbolKind::Trait
                        && psym.type_index != type_pool::TypeIndex::INVALID
                    {
                        parent_indices.push(psym.type_index);
                    }
                }
            }
        }

        // Store parents in the TypeKind.
        if !parent_indices.is_empty() {
            if let TypeKind::Trait { parents, .. } = &mut r.type_pool.get_mut(type_index).kind {
                *parents = parent_indices.clone();
            }
        }

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

    for node_idx in impl_trait_nodes {
        // ImplTraitDef / ExtendTraitDef: [0] trait_expr  [1] type_expr  multi: members
        let children = ast.fixed_children(node_idx);
        let trait_node = children[0];
        let type_node = children[1];

        // Look up the trait's type index via its resolved symbol.
        let trait_ti = r.node_symbols.get(&trait_node).and_then(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Trait && sym.type_index != type_pool::TypeIndex::INVALID {
                Some(sym.type_index)
            } else {
                None
            }
        });

        // Look up the target type's type index.
        let target_ti = r.node_symbols.get(&type_node).and_then(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.type_index != type_pool::TypeIndex::INVALID {
                Some(sym.type_index)
            } else {
                None
            }
        });

        let (Some(trait_type), Some(implementor)) = (trait_ti, target_ti) else {
            continue;
        };

        // ── Orphan rule check (impl only, not extend) ─────────────
        // `impl Trait for Type` requires the current package to own
        // either `Trait` or `Type`.  `extend` bypasses this rule.
        // TODO: enforce once package management is integrated with
        //       resolution (currently single-file, no package ids).

        // Determine scope restriction: extend trait impls are scope-local.
        let is_extend = ast.node(node_idx).kind == NodeKind::ExtendTraitDef;
        let visible_scope = if is_extend {
            r.scopes
                .iter()
                .find(|s| s.node == node_idx)
                .and_then(|s| s.parent)
                .map(|sid| sid.0)
        } else {
            None
        };

        // Collect method implementations from the impl block's members.
        let mut methods = Vec::new();
        let mut provided_names: Vec<str_interner::StrId> = Vec::new();
        for &member in ast.multi_children(node_idx) {
            if ast.node(member).kind == NodeKind::FunctionDef {
                let fn_children = ast.fixed_children(member);
                let fn_name = ast.node(fn_children[0]).str_id;

                let func_id = r
                    .node_symbols
                    .get(&fn_children[0])
                    .map(|&sym_id| sym_id.0)
                    .unwrap_or(0);

                methods.push(MethodSlot {
                    name: fn_name,
                    func_id,
                    trait_impl: Some(trait_type),
                    visible_scope,
                });

                r.type_pool.add_method(
                    implementor,
                    MethodSlot {
                        name: fn_name,
                        func_id,
                        trait_impl: Some(trait_type),
                        visible_scope,
                    },
                );

                provided_names.push(fn_name);
            }
        }

        // ── Validate required methods & inject defaults ───────────
        if let Some(trait_info) = r.traits.iter().find(|ti| ti.type_index == trait_type) {
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

            if let Some(trait_node) = trait_def_node {
                if !trait_node.is_null() {
                    // Find the scope for this trait definition.
                    let trait_scope = r.scopes.iter().find(|s| s.node == trait_node);

                    if let Some(scope) = trait_scope {
                        let bindings = scope.bindings.clone();
                        for &derived_name in &derived {
                            if !provided_names.contains(&derived_name) {
                                // Use the trait's default function symbol.
                                if let Some(&sym_id) = bindings.get(&derived_name) {
                                    let default_func_id = sym_id.0;
                                    let slot = MethodSlot {
                                        name: derived_name,
                                        func_id: default_func_id,
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
            }

            // ── Validate parent trait impls ────────────────────────
            let parent_traits = trait_info.parent_traits.clone();
            for &parent_ti in &parent_traits {
                if !r.type_pool.has_trait_impl(implementor, parent_ti) {
                    let span = ast.node(node_idx).span;
                    let parent_name = match &r.type_pool.get(parent_ti).kind {
                        TypeKind::Trait { name, .. } => str_interner::get(*name),
                        _ => "?".to_string(),
                    };
                    let trait_name = match &r.type_pool.get(trait_type).kind {
                        TypeKind::Trait { name, .. } => str_interner::get(*name),
                        _ => "?".to_string(),
                    };
                    let mut diag = diagnostic::Diagnostic::error(format!(
                        "impl of trait `{trait_name}` requires parent trait `{parent_name}` to also be implemented",
                    ));
                    diag.primary_span = Some(span);
                    r.diagnostics.push(diag);
                }
            }
        }

        // Record the trait implementation.
        r.type_pool.add_trait_impl(TraitImplRecord {
            trait_type,
            implementor,
            methods,
        });
    }

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
            kind == NodeKind::ImplDef || kind == NodeKind::ExtendDef
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
            if sym.type_index != type_pool::TypeIndex::INVALID {
                Some(sym.type_index)
            } else {
                None
            }
        });

        let Some(implementor) = target_ti else {
            continue;
        };

        for &member in ast.multi_children(node_idx) {
            if ast.node(member).kind == NodeKind::FunctionDef {
                let fn_children = ast.fixed_children(member);
                let fn_name = ast.node(fn_children[0]).str_id;

                let func_id = r
                    .node_symbols
                    .get(&fn_children[0])
                    .map(|&sym_id| sym_id.0)
                    .unwrap_or(0);

                r.type_pool.add_method(
                    implementor,
                    MethodSlot {
                        name: fn_name,
                        func_id,
                        trait_impl: None,
                        visible_scope,
                    },
                );
            }
        }
    }

    // ── Process derive definitions ─────────────────────────────────
    //
    // `derive Eq, Show for Point` auto-registers trait impls.
    // We collect DeriveDef nodes from the AST (they are NOT scope nodes).
    let derive_nodes: Vec<_> = (1..ast.nodes.len())
        .map(|i| NodeIndex(i as u32))
        .filter(|&idx| ast.node(idx).kind == NodeKind::DeriveDef)
        .collect();

    for node_idx in derive_nodes {
        // DeriveDef: [0] type_expr  multi = trait_exprs
        let children = ast.fixed_children(node_idx);
        let type_node = children[0];

        let target_ti = r.node_symbols.get(&type_node).and_then(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.type_index != type_pool::TypeIndex::INVALID {
                Some(sym.type_index)
            } else {
                None
            }
        });

        let Some(implementor) = target_ti else {
            continue;
        };

        for &trait_expr in ast.multi_children(node_idx) {
            let trait_ti = r.node_symbols.get(&trait_expr).and_then(|&sym_id| {
                let sym = &r.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Trait && sym.type_index != type_pool::TypeIndex::INVALID
                {
                    Some(sym.type_index)
                } else {
                    None
                }
            });

            let Some(trait_type) = trait_ti else {
                continue;
            };

            // Generate synthetic method slots for well-known derivable traits.
            let methods = generate_derive_methods(r, implementor, trait_type);

            for slot in &methods {
                r.type_pool.add_method(implementor, slot.clone());
            }

            r.type_pool.add_trait_impl(TraitImplRecord {
                trait_type,
                implementor,
                methods,
            });
        }
    }

    // ── Build vtables for all recorded trait impls ─────────────────
    build_vtables(r);
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
    // Collect method order for each trait.
    let trait_method_orders: Vec<(type_pool::TypeIndex, Vec<str_interner::StrId>)> = r
        .traits
        .iter()
        .map(|ti| {
            let mut order = Vec::new();
            // Add parent trait methods first (recursively).
            for &parent_ti in &ti.parent_traits {
                if let Some(parent_info) = r.traits.iter().find(|t| t.type_index == parent_ti) {
                    for m in &parent_info.required_methods {
                        if !order.contains(m) {
                            order.push(*m);
                        }
                    }
                    for m in &parent_info.derived_methods {
                        if !order.contains(m) {
                            order.push(*m);
                        }
                    }
                }
            }
            // Then this trait's own methods.
            for m in &ti.required_methods {
                if !order.contains(m) {
                    order.push(*m);
                }
            }
            for m in &ti.derived_methods {
                if !order.contains(m) {
                    order.push(*m);
                }
            }
            (ti.type_index, order)
        })
        .collect();

    // Build a vtable for every trait impl record.
    let impl_records: Vec<_> = r
        .type_pool
        .trait_impls_snapshot()
        .iter()
        .map(|rec| (rec.trait_type, rec.implementor, rec.methods.clone()))
        .collect();

    for (trait_type, implementor, methods) in impl_records {
        let method_order = trait_method_orders
            .iter()
            .find(|(ti, _)| *ti == trait_type)
            .map(|(_, order)| order.as_slice());

        let Some(order) = method_order else {
            continue;
        };

        let mut entries = Vec::with_capacity(order.len());
        for &method_name in order {
            let func_id = methods
                .iter()
                .find(|m| m.name == method_name)
                .map(|m| m.func_id)
                .unwrap_or(0); // 0 = unresolved placeholder
            entries.push(func_id);
        }

        r.type_pool.add_vtable(VTable {
            trait_type,
            implementor,
            entries,
        });
    }
}

// ---------------------------------------------------------------------------
// Derive method generation
// ---------------------------------------------------------------------------

/// Sentinel func_id indicating a compiler-derived method.
/// Re-exported from type_pool for use by the interpreter.
pub const DERIVE_FUNC_ID: u32 = type_pool::DERIVE_FUNC_ID;

fn generate_derive_methods(
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
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    } else if trait_type == wk.ord || trait_type == wk.partial_ord {
        methods.push(MethodSlot {
            name: str_interner::intern("cmp"),
            func_id: DERIVE_FUNC_ID,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    } else if trait_type == wk.hash {
        methods.push(MethodSlot {
            name: str_interner::intern("hash"),
            func_id: DERIVE_FUNC_ID,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    } else if trait_type == wk.display {
        methods.push(MethodSlot {
            name: str_interner::intern("to_string"),
            func_id: DERIVE_FUNC_ID,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    }

    methods
}
