//! Phase 3d: Trait & Method Resolution
//!
//! Collects [`TraitInfo`] entries (required / derived methods) from all
//! trait symbols registered by Phase 3a.  Also records impl-trait
//! relationships discovered from `ImplTraitDef` / `ExtendTraitDef` nodes.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{MethodSlot, TraitImplRecord, TypeIndex, TypeKind, VTable};

use crate::resolver::Resolver;
use crate::{ScopeId, SymbolKind, TraitInfo};

fn denoted_type(r: &Resolver<'_>, node: NodeIndex) -> Option<TypeIndex> {
    r.node_type_values
        .get(&node)
        .copied()
        .or_else(|| {
            r.node_symbols
                .get(&node)
                .map(|symbol| r.symbols[symbol.0 as usize].type_index)
        })
        .and_then(|ty| r.type_pool.canonical_type(ty))
}

fn type_defining_package(
    r: &Resolver<'_>,
    ast: &Ast,
    expr_node: NodeIndex,
    canon_ty: TypeIndex,
    fallback_factory_pkg: u32,
) -> Option<u32> {
    if let Some(sym) = r.symbols.iter().find(|sym| {
        !sym.def_node.is_null()
            && matches!(
                ast.node(sym.def_node).kind,
                NodeKind::StructDef
                    | NodeKind::EnumDef
                    | NodeKind::Newtype
                    | NodeKind::TraitDef
                    | NodeKind::EffectDef
                    | NodeKind::AsyncEffectDef
                    | NodeKind::ModuleDef
            )
            && r.type_pool.canonical_type(sym.type_index) == Some(canon_ty)
    }) {
        return Some(crate::imports::package_id(r, sym.scope));
    }
    if matches!(r.type_pool.get(canon_ty).kind, TypeKind::Tuple { .. })
        && let Some(&sym_id) = r.node_symbols.get(&expr_node)
    {
        let sym = &r.symbols[sym_id.0 as usize];
        if !sym.def_node.is_null() && ast.node(sym.def_node).kind == NodeKind::Typealias {
            return Some(crate::imports::package_id(r, sym.scope));
        }
    }
    if matches!(
        r.type_pool.checked_iteration_step_item(canon_ty),
        Ok(Some(_))
    ) {
        return Some(fallback_factory_pkg);
    }
    None
}

fn node_package_id(r: &Resolver<'_>, node_idx: NodeIndex) -> u32 {
    let scope = r
        .imports
        .module_scopes
        .get(&node_idx)
        .and_then(|&sid| r.scopes[sid.0 as usize].parent)
        .or_else(|| r.node_scopes.get(&node_idx).copied())
        .unwrap_or(ScopeId::ROOT);
    crate::imports::package_id(r, scope)
}

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
        .symbols
        .iter()
        .filter_map(|sym| {
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
        if ast.node(node_idx).kind == NodeKind::ImplTraitDef
            && !r.options.builtin_access
            && !r.options.privileged_nodes.contains(&node_idx)
            && r.type_pool.as_intrinsic(implementor).is_none()
            && !r.type_pool.is_reserved_collection_role(implementor)
        {
            let impl_pkg = node_package_id(r, node_idx);
            let trait_pkg = type_defining_package(r, ast, trait_node, trait_type, impl_pkg);
            let type_pkg = type_defining_package(r, ast, type_node, implementor, impl_pkg);
            if trait_pkg != Some(impl_pkg) && type_pkg != Some(impl_pkg) {
                r.diag_ctx
                    .error(
                        "orphan impl: either the implemented trait or the target type must be defined in the current package"
                            .into(),
                    )
                    .with_primary_span(ast.node(node_idx).span)
                    .emit(r.diag_ctx);
                continue;
            }
        }
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

        let target_ti = r
            .scopes
            .iter()
            .find(|s| s.node == node_idx)
            .and_then(|s| s.assoc_type)
            .and_then(|ty| r.type_pool.canonical_type(ty))
            .or_else(|| {
                r.node_symbols.get(&type_node).and_then(|&sym_id| {
                    let sym = &r.symbols[sym_id.0 as usize];
                    r.type_pool.canonical_type(sym.type_index)
                })
            });

        let Some(implementor) = target_ti else {
            continue;
        };

        if ast.node(node_idx).kind == NodeKind::ImplDef
            && !r.options.builtin_access
            && !r.options.privileged_nodes.contains(&node_idx)
            && r.type_pool.as_intrinsic(implementor).is_none()
            && !r.type_pool.is_reserved_collection_role(implementor)
        {
            let impl_pkg = node_package_id(r, node_idx);
            let type_pkg = type_defining_package(r, ast, type_node, implementor, impl_pkg);
            if type_pkg != Some(impl_pkg) {
                r.diag_ctx
                    .error(
                        "orphan impl: cannot implement inherent methods for a type defined in another package"
                            .into(),
                    )
                    .with_primary_span(ast.node(node_idx).span)
                    .emit(r.diag_ctx);
                continue;
            }
        }

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

    // ── Check orphan rules on DeriveDef & instantiate custom user trait derivations ──
    for index in 1..ast.nodes.len() {
        let node = NodeIndex(index as u32);
        if ast.node(node).kind != NodeKind::DeriveDef {
            continue;
        }
        let type_node = ast.fixed_children(node)[0];
        let Some(implementor) = denoted_type(r, type_node) else {
            continue;
        };
        if r.type_pool.is_reserved_collection_role(implementor) {
            continue;
        }
        for &trait_node in ast.multi_children(node) {
            let Some(trait_type) = denoted_type(r, trait_node) else {
                continue;
            };
            if !r.options.builtin_access
                && !r.options.privileged_nodes.contains(&node)
                && r.type_pool.as_intrinsic(implementor).is_none()
            {
                let derive_pkg = node_package_id(r, node);
                let trait_pkg = type_defining_package(r, ast, trait_node, trait_type, derive_pkg);
                let type_pkg = type_defining_package(r, ast, type_node, implementor, derive_pkg);
                if trait_pkg != Some(derive_pkg) && type_pkg != Some(derive_pkg) {
                    r.diag_ctx
                        .error(
                            "orphan derive: either the derived trait or the target type must be defined in the current package"
                                .into(),
                        )
                        .with_primary_span(ast.node(node).span)
                        .emit(r.diag_ctx);
                    continue;
                }
            }
            let wk = &r.type_pool.well_known;
            let is_builtin_derive_trait = [
                wk.eq,
                wk.partial_eq,
                wk.ord,
                wk.partial_ord,
                wk.display,
                wk.hash,
                wk.iterator,
                wk.into_iterator,
                wk.json,
            ]
            .contains(&trait_type);
            if is_builtin_derive_trait {
                continue;
            }
            let Some(trait_info) = r
                .traits
                .iter()
                .find(|ti| ti.type_index == trait_type)
                .cloned()
            else {
                continue;
            };
            if !r.options.builtin_access
                && !r.options.privileged_nodes.contains(&node)
                && r.type_pool.as_intrinsic(implementor).is_some()
            {
                r.diag_ctx
                    .error(
                        "cannot derive traits on a builtin type outside a privileged package (std/core/alloc)"
                            .into(),
                    )
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
                continue;
            }
            if !implementations.insert((implementor, trait_type, None))
                || r.type_pool.has_trait_impl(implementor, trait_type)
            {
                r.diag_ctx
                    .error("cannot derive a trait already implemented for this type".into())
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
                continue;
            }
            if !trait_info.required_methods.is_empty() {
                for &req_name in &trait_info.required_methods {
                    r.diag_ctx
                        .error(format!(
                            "cannot derive trait `{}`: required method `{}` has no `derive fn` default",
                            str_interner::get(trait_info.name),
                            str_interner::get(req_name),
                        ))
                        .with_primary_span(ast.node(trait_node).span)
                        .emit(r.diag_ctx);
                }
                continue;
            }
            let has_assoc = crate::associated_types::has_declarations(r, trait_type);
            if has_assoc {
                r.type_pool.add_trait_impl(TraitImplRecord {
                    trait_type,
                    implementor,
                    methods: Vec::new(),
                    visible_scope: None,
                });
                crate::associated_types::install(r, ast, node, implementor, trait_type, None);
            }
            let mut methods = Vec::new();
            let trait_def_node = r
                .symbols
                .iter()
                .find(|s| s.kind == SymbolKind::Trait && s.type_index == trait_type)
                .map(|s| s.def_node);
            if let Some(t_def) = trait_def_node
                && !t_def.is_null()
                && let Some(scope) = r.scopes.iter().find(|s| s.node == t_def)
            {
                let bindings = scope.bindings.clone();
                let provider_scope = r
                    .node_scopes
                    .get(&node)
                    .map(|s| s.0)
                    .unwrap_or(ScopeId::ROOT.0);
                for &derived_name in &trait_info.derived_methods {
                    if let Some(&sym_id) = bindings.get(&derived_name) {
                        match crate::default_methods::register(
                            r,
                            ast,
                            sym_id,
                            implementor,
                            trait_type,
                            None,
                            provider_scope,
                        ) {
                            Ok(function) => {
                                let slot = MethodSlot {
                                    name: derived_name,
                                    func_id: function.0,
                                    access: crate::access::method_access(r, sym_id),
                                    trait_impl: Some(trait_type),
                                    visible_scope: None,
                                };
                                methods.push(slot.clone());
                                r.type_pool.add_method(implementor, slot);
                            }
                            Err(message) => {
                                r.diag_ctx
                                    .error(message)
                                    .with_primary_span(ast.node(node).span)
                                    .emit(r.diag_ctx);
                            }
                        }
                    }
                }
            }
            for &parent in &trait_info.parent_traits {
                parent_checks.push((trait_node, implementor, trait_type, parent, None));
            }
            let record = TraitImplRecord {
                trait_type,
                implementor,
                methods,
                visible_scope: None,
            };
            if has_assoc {
                if let Err(error) = r.type_pool.complete_trait_impl(record) {
                    r.diag_ctx
                        .error(error.to_string())
                        .with_primary_span(ast.node(node).span)
                        .emit(r.diag_ctx);
                }
            } else {
                r.type_pool.add_trait_impl(record);
            }
        }
    }

    crate::comparison_derivation::register(r, ast);
    crate::ordering::complete_records(r, ast);
    crate::json_derivation::complete_records(r, ast);

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
    crate::hash_derivation::validate(r, ast);
    crate::json_derivation::validate(r, ast);
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
    } else if trait_type == wk.json {
        methods.push(MethodSlot {
            name: str_interner::intern("to_json"),
            func_id: DERIVE_FUNC_ID,
            access: type_pool::MethodAccess::Public,
            trait_impl: Some(trait_type),
            visible_scope: None,
        });
    }

    methods
}
