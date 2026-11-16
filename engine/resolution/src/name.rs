//! Phase 3a: Name Resolution
//!
//! Walks the AST, builds the scope tree, and resolves all identifier bindings.
//! After this phase every identifier node is mapped to a [`SymbolId`] and every
//! scope-introducing construct has a corresponding [`Scope`] entry.
//!
//! Top-level definitions (functions, types, traits, effects) are
//! forward-declared before their bodies are resolved, so declaration order
//! within a file scope does not matter.

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::Level;
use type_pool::TypeKind;

use crate::resolver::Resolver;
use crate::{SymbolKind, Visibility};

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Walk `node_idx` and all descendants, building scopes and binding names.
pub(crate) fn resolve_names(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    if node_idx.is_null() {
        return;
    }
    r.node_scopes.insert(node_idx, r.current_scope);
    let kind = ast.node(node_idx).kind;
    let str_id = ast.node(node_idx).str_id;

    match kind {
        // ── Leaf identifiers ───────────────────────────────────────
        NodeKind::Id | NodeKind::SelfLower => resolve_id(r, ast, node_idx, str_id),
        NodeKind::SelfUpper => crate::associated::resolve_self_type(r, ast, node_idx),

        // ── Scope blocks ───────────────────────────────────────────
        NodeKind::Block => {
            r.push_scope(node_idx, None);
            // Function names are available throughout this lexical block, while
            // local values still enter the environment in source order.
            for &child in ast.multi_children(node_idx) {
                if ast.node(child).kind == NodeKind::FunctionDef {
                    forward_declare_one(r, ast, child);
                }
            }
            // Local extensions have independent declaration scopes, but their
            // availability covers this block and its descendants. Prepare only
            // extensions here; ordinary local values remain source-ordered.
            for &child in ast.multi_children(node_idx) {
                let inner = if matches!(
                    ast.node(child).kind,
                    NodeKind::PubDef | NodeKind::PrivateDef
                ) {
                    ast.fixed_children(child)[0]
                } else {
                    child
                };
                if matches!(
                    ast.node(child).kind,
                    NodeKind::ExtendDef | NodeKind::ExtendTraitDef
                ) {
                    prepare_scope_tree(r, ast, child, None);
                } else if !inner.is_null() && ast.node(inner).kind == NodeKind::DeriveDef {
                    r.pending_derives.push((r.current_scope, inner));
                }
            }
            crate::associated::attach(r, ast);
            for &child in ast.multi_children(node_idx) {
                resolve_names(r, ast, child);
            }
            r.pop_scope();
        }

        NodeKind::FileScope => {
            if !r.imports.prepared_scopes.contains(&node_idx) {
                prepare_scope_tree(r, ast, node_idx, None);
                crate::imports::resolve_all(r, ast);
            }
            let original = r.current_scope;
            r.current_scope = r.imports.module_scopes[&node_idx];
            // Pass 2: fully resolve names and bodies.
            for &child in ast.multi_children(node_idx) {
                resolve_names(r, ast, child);
            }
            r.current_scope = original;
        }

        // ── Variable declarations ──────────────────────────────────
        NodeKind::LetDecl | NodeKind::ConstDecl => resolve_let_or_const(r, ast, node_idx, kind),
        NodeKind::VarDecl => resolve_var_decl(r, ast, node_idx),
        NodeKind::GlobalDecl => resolve_global_decl(r, ast, node_idx),
        NodeKind::AssocDecl => crate::associated::resolve_declaration(r, ast, node_idx),
        NodeKind::OptionPropagation | NodeKind::ErrorPropagation => {
            validate_control_placement(r, ast, node_idx);
            resolve_children(r, ast, node_idx);
        }
        NodeKind::ReturnStatement
        | NodeKind::ResumeStatement
        | NodeKind::BreakStatement
        | NodeKind::ContinueStatement => {
            validate_control_placement(r, ast, node_idx);
            resolve_children(r, ast, node_idx);
        }
        NodeKind::Assign
        | NodeKind::AddAssign
        | NodeKind::SubAssign
        | NodeKind::MulAssign
        | NodeKind::DivAssign
        | NodeKind::ModAssign => {
            for &child in ast.fixed_children(node_idx) {
                resolve_names(r, ast, child);
            }
            let target = ast.fixed_children(node_idx)[0];
            if let Some(&symbol) = r.node_symbols.get(&target) {
                let definition = &r.symbols[symbol.0 as usize];
                if definition.kind == SymbolKind::Constant
                    || (!definition.def_node.is_null()
                        && ast.node(definition.def_node).kind == NodeKind::ConstDecl)
                {
                    r.diag_ctx
                        .error(format!(
                            "cannot assign to constant `{}`",
                            str_interner::get(definition.name)
                        ))
                        .with_primary_span(ast.node(target).span)
                        .emit(r.diag_ctx);
                }
            }
        }

        // ── Function definition ────────────────────────────────────
        NodeKind::FunctionDef => resolve_function_def(r, ast, node_idx),

        // ── Type definitions ───────────────────────────────────────
        NodeKind::StructDef => resolve_struct_def(r, ast, node_idx),
        NodeKind::EnumDef => resolve_enum_def(r, ast, node_idx),
        NodeKind::EnumVariant => resolve_enum_variant(r, ast, node_idx, 0),
        NodeKind::StructField => resolve_struct_field(r, ast, node_idx),

        // ── Trait / effect / module ────────────────────────────────
        NodeKind::TraitDef => resolve_trait_def(r, ast, node_idx),
        NodeKind::TraitDefFn | NodeKind::TraitDeriveFn => {
            resolve_trait_fn(r, ast, node_idx, kind);
        }
        NodeKind::EffectDef | NodeKind::AsyncEffectDef => {
            resolve_effect_def(r, ast, node_idx, kind);
        }
        NodeKind::EffectElimination => resolve_handler_names(r, ast, node_idx),
        NodeKind::ModuleDef => resolve_module_def(r, ast, node_idx),

        // ── Impl / extend blocks ───────────────────────────────────
        NodeKind::ImplDef | NodeKind::ImplTraitDef => resolve_impl(r, ast, node_idx),
        NodeKind::ExtendDef | NodeKind::ExtendTraitDef => resolve_extend(r, ast, node_idx),

        // ── Typealias & Newtype ────────────────────────────────────
        NodeKind::Typealias => resolve_typealias(r, ast, node_idx),
        NodeKind::Newtype => resolve_newtype(r, ast, node_idx),

        // ── View (e.g. `.print'builtin`) ───────────────────────────
        NodeKind::View => resolve_view(r, ast, node_idx),

        // ── Standalone symbol literal `.id` ────────────────────────
        NodeKind::Symbol => {
            r.diag_ctx
                .error(
                    "standalone symbol literals `.name` are not supported as runtime values".into(),
                )
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
        }

        // ── Unsupported surface constructs ─────────────────────────
        NodeKind::ResetExpr => {
            r.diag_ctx
                .error(
                    "`reset` expressions are not supported; declare an `effect` with `catch k` instead"
                        .into(),
                )
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
            let children = ast.fixed_children(node_idx);
            resolve_names(r, ast, children[0]);
            resolve_names(r, ast, children[1]);
        }
        NodeKind::ShiftExpr => {
            r.diag_ctx
                .error(
                    "`shift` expressions are not supported; declare an `effect` with `catch k` instead"
                        .into(),
                )
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
            let children = ast.fixed_children(node_idx);
            resolve_names(r, ast, children[0]);
            r.push_scope(node_idx, None);
            if !children[1].is_null() && ast.node(children[1]).kind == NodeKind::Id {
                bind_pattern_identifier(r, ast, children[1], NodeKind::LetDecl);
            }
            resolve_names(r, ast, children[2]);
            r.pop_scope();
        }
        NodeKind::Object => {
            r.diag_ctx
                .error(
                    "anonymous Object literals `{ ... }` are not supported; define a `struct` or use `Map`"
                        .into(),
                )
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
            for &item in ast.multi_children(node_idx) {
                if ast.node(item).kind == NodeKind::Property {
                    resolve_names(r, ast, ast.fixed_children(item)[1]);
                } else {
                    resolve_names(r, ast, item);
                }
            }
        }
        NodeKind::Await => {
            resolve_names(r, ast, ast.fixed_children(node_idx)[0]);
        }

        // ── Derive ─────────────────────────────────────────────────
        NodeKind::DeriveDef => resolve_derive(r, ast, node_idx),

        // ── Visibility wrapper ─────────────────────────────────────
        NodeKind::PubDef | NodeKind::PrivateDef => resolve_pub_def(r, ast, node_idx),

        // ── Use statement ──────────────────────────────────────────
        NodeKind::UseStatement => {
            crate::imports::resolve_local_use(r, ast, node_idx, Visibility::Package);
        }

        // ── Lambda & case closure ──────────────────────────────────
        NodeKind::Lambda => resolve_lambda(r, ast, node_idx),
        NodeKind::CaseMap | NodeKind::CaseAlternative => resolve_case_closure(r, ast, node_idx),
        NodeKind::DeferStatement => resolve_defer(r, ast, node_idx),

        // ── Named argument / property assignment / spread ──────────
        NodeKind::NamedArg | NodeKind::PropertyAssignment => {
            // children[0] = parameter name (Id) — do NOT resolve as variable
            // children[1] = value expression — resolve normally
            let children = ast.fixed_children(node_idx);
            resolve_names(r, ast, children[1]);
        }
        NodeKind::SpreadArg => {
            resolve_names(r, ast, ast.fixed_children(node_idx)[0]);
        }

        // ── Control flow ───────────────────────────────────────────
        NodeKind::PostMatch | NodeKind::ErrorElimination => {
            crate::enums::resolve_match_names(r, ast, node_idx)
        }
        NodeKind::BoolMatches => {
            let children = ast.fixed_children(node_idx);
            resolve_names(r, ast, children[0]);
            let previous = r.current_scope;
            r.push_scope(node_idx, None);
            resolve_pattern(r, ast, children[1], NodeKind::CaseArm);
            r.current_scope = previous;
        }
        NodeKind::ForLoop => resolve_for_loop(r, ast, node_idx),
        NodeKind::WhileLoop => resolve_while_loop(r, ast, node_idx),
        NodeKind::IfStatement => resolve_if(r, ast, node_idx),
        NodeKind::WhenStatement => resolve_when(r, ast, node_idx),
        NodeKind::InfixFnCall => resolve_infix_fn_call(r, ast, node_idx),

        // ── Effect handler ─────────────────────────────────────────
        NodeKind::HandlesStatement => resolve_handles(r, ast, node_idx),
        NodeKind::LetHandler => resolve_let_handler_names(r, ast, node_idx),

        // ── Qualified access ───────────────────────────────────────
        NodeKind::Projection => resolve_projection(r, ast, node_idx),
        NodeKind::Call => {
            resolve_children(r, ast, node_idx);
            crate::associated::resolve_constructor(r, ast, node_idx);
        }
        NodeKind::PropertyPattern => resolve_property_pattern(r, ast, node_idx),

        // ── Struct / enum construction: TypeName { field: val, ... }
        NodeKind::ExtendedCall => {
            let children = ast.fixed_children(node_idx);
            // children[0] = type name (e.g. `Student`) — resolve as a type symbol
            resolve_names(r, ast, children[0]);
            // multi = Property nodes (field: value) or plain expression args
            for &arg in ast.multi_children(node_idx) {
                if ast.node(arg).kind == NodeKind::Property {
                    // Property: [0] = field name (NOT a variable — skip it)
                    //           [1] = value expression — resolve normally
                    let prop_children = ast.fixed_children(arg);
                    if prop_children.len() > 1 {
                        resolve_names(r, ast, prop_children[1]);
                    }
                } else {
                    resolve_names(r, ast, arg);
                }
            }
        }

        // ── Default: recurse ───────────────────────────────────────
        _ => resolve_children(r, ast, node_idx),
    }
}

fn resolve_handler_names(r: &mut Resolver, ast: &Ast, node: NodeIndex) {
    if ast.multi_children(node).len() > 31 {
        r.diag_ctx
            .error("an effect elimination currently supports at most 31 handlers".into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
    resolve_names(r, ast, ast.fixed_children(node)[0]);
    let mut seen = std::collections::HashSet::new();
    for &arm in ast.multi_children(node) {
        let children = ast.fixed_children(arm);
        let raw_pattern = children[0];
        let (pattern, arm_is_async) = crate::effect::unwrap_handler_pattern(ast, raw_pattern);
        let span = ast.node(raw_pattern).span;
        if pattern.is_null() || ast.node(pattern).kind != NodeKind::PatternCall {
            r.diag_ctx
                .error("an effect handler arm must name an effect and its parameters".into())
                .with_primary_span(span)
                .emit(r.diag_ctx);
            continue;
        }
        let callee = ast.fixed_children(pattern)[0];
        resolve_names(r, ast, callee);
        let Some(&symbol) = r.node_symbols.get(&callee) else {
            continue;
        };
        let effect = &r.symbols[symbol.0 as usize];
        if effect.kind != SymbolKind::Effect {
            r.diag_ctx
                .error("handler arm target must be an effect".into())
                .with_primary_span(span)
                .emit(r.diag_ctx);
            continue;
        }
        let effect_name = str_interner::get(effect.name);
        let def_is_async = !effect.def_node.is_null()
            && ast.node(effect.def_node).kind == NodeKind::AsyncEffectDef;
        if def_is_async && !arm_is_async {
            r.diag_ctx
                .error(format!(
                    "handler for `async effect {effect_name}` must be marked `async`"
                ))
                .with_primary_span(span)
                .emit(r.diag_ctx);
        } else if !def_is_async && arm_is_async {
            r.diag_ctx
                .error(format!(
                    "`async` handler arm cannot handle synchronous effect `{effect_name}`"
                ))
                .with_primary_span(span)
                .emit(r.diag_ctx);
        }
        if !seen.insert(symbol) {
            r.diag_ctx
                .error("duplicate handler for the same effect".into())
                .with_primary_span(span)
                .emit(r.diag_ctx);
        }
        let def_params = ast.multi_children(effect.def_node);
        let expected = def_params.len();
        let params = ast.multi_children(pattern);
        if params.len() != expected {
            r.diag_ctx
                .error(format!(
                    "handler expects {expected} parameters, found {}",
                    params.len()
                ))
                .with_primary_span(span)
                .emit(r.diag_ctx);
        }
        r.handler_arms.insert(arm);
        r.push_scope(arm, None);
        for (idx, &param) in params.iter().enumerate() {
            let is_catch = def_params
                .get(idx)
                .is_some_and(|&def_p| ast.node(def_p).kind == NodeKind::ParamCatch);
            if is_catch {
                if !matches!(ast.node(param).kind, NodeKind::Id | NodeKind::Underscore) {
                    r.diag_ctx
                        .error("continuation parameter cannot be destructured".into())
                        .with_primary_span(ast.node(param).span)
                        .emit(r.diag_ctx);
                }
                resolve_pattern_with_mode(r, ast, param, NodeKind::LetDecl, false);
            } else {
                resolve_pattern_with_mode(r, ast, param, NodeKind::LetDecl, true);
            }
        }
        resolve_names(r, ast, children[1]);
        r.pop_scope();
    }
}

// ---------------------------------------------------------------------------
// Forward declaration
// ---------------------------------------------------------------------------

/// Pre-scan all direct children of a scope-introducing node and register
/// top-level definitions (functions, types, traits, effects, modules).
/// This makes declaration order irrelevant within a file scope.
fn forward_declare_scope(r: &mut Resolver, ast: &Ast, scope_node: NodeIndex) {
    for &child in ast.multi_children(scope_node) {
        forward_declare_one(r, ast, child);
    }
}

/// Allocate all module scopes before imports or function bodies are resolved.
fn prepare_scope_tree(
    r: &mut Resolver,
    ast: &Ast,
    node: NodeIndex,
    associated: Option<type_pool::TypeIndex>,
) {
    let original = r.current_scope;
    let scope = r.push_scope(node, associated);
    r.imports.module_scopes.insert(node, scope);
    if matches!(
        ast.node(node).kind,
        NodeKind::FileScope | NodeKind::ModuleDef
    ) {
        r.imports.namespace_scopes.insert(scope);
    }
    r.imports.prepared_scopes.insert(node);
    if matches!(
        ast.node(node).kind,
        NodeKind::ExtendDef | NodeKind::ExtendTraitDef
    ) {
        r.imports.extension_scopes.insert(scope, original);
    }
    forward_declare_scope(r, ast, node);
    let mut variant_index = 0;
    for &child in ast.multi_children(node) {
        let (inner, visibility) = if matches!(
            ast.node(child).kind,
            NodeKind::PubDef | NodeKind::PrivateDef
        ) {
            (
                ast.fixed_children(child)[0],
                if ast.node(child).kind == NodeKind::PrivateDef {
                    Visibility::Private
                } else {
                    Visibility::Public
                },
            )
        } else {
            (child, Visibility::Package)
        };
        match ast.node(inner).kind {
            NodeKind::EnumVariant => {
                let name = ast.fixed_children(inner)[0];
                if let Some(&symbol) = r.node_symbols.get(&name) {
                    r.enum_variant_indices.insert(symbol, variant_index);
                }
                variant_index += 1;
            }
            NodeKind::ModuleDef | NodeKind::StructDef | NodeKind::EnumDef => {
                let name = ast.fixed_children(inner)[0];
                if let Some(&symbol) = r.node_symbols.get(&name) {
                    let ty = r.symbols[symbol.0 as usize].type_index;
                    prepare_scope_tree(r, ast, inner, Some(ty));
                }
            }
            NodeKind::ImplDef
            | NodeKind::ImplTraitDef
            | NodeKind::ExtendDef
            | NodeKind::ExtendTraitDef => prepare_scope_tree(r, ast, inner, None),
            NodeKind::UseStatement => r.imports.uses.push((scope, inner, visibility)),
            NodeKind::DeriveDef => r.pending_derives.push((scope, inner)),
            _ => {}
        }
    }
    r.current_scope = original;
}

fn forward_declare_one(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    if node_idx.is_null() {
        return;
    }
    let kind = ast.node(node_idx).kind;
    if matches!(
        kind,
        NodeKind::FunctionDef
            | NodeKind::StructDef
            | NodeKind::EnumDef
            | NodeKind::Newtype
            | NodeKind::TraitDef
            | NodeKind::EffectDef
            | NodeKind::AsyncEffectDef
            | NodeKind::ModuleDef
            | NodeKind::Typealias
            | NodeKind::ConstDecl
            | NodeKind::LetDecl
            | NodeKind::VarDecl
            | NodeKind::StructField
            | NodeKind::EnumVariant
            | NodeKind::AssocBinding
    ) {
        let name = ast.fixed_children(node_idx)[0];
        if !r.options.detached_package_roots.contains(&node_idx)
            && ast.node(name).kind == NodeKind::Id
            && let Some(existing) = r.lookup_current_scope(ast.node(name).str_id)
        {
            let previous = r.symbols[existing.0 as usize].def_node;
            if previous != node_idx {
                r.diag_ctx
                    .error(format!(
                        "duplicate definition `{}`",
                        str_interner::get(ast.node(name).str_id)
                    ))
                    .with_primary_span(ast.node(name).span)
                    .emit(r.diag_ctx);
            }
            return;
        }
    }
    match kind {
        NodeKind::AssocBinding => {
            let name = ast.fixed_children(node_idx)[0];
            r.define_symbol(
                ast.node(name).str_id,
                SymbolKind::Type,
                node_idx,
                Visibility::Package,
            );
        }
        NodeKind::FunctionDef => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            let symbol = r.define_symbol(
                name_str,
                SymbolKind::Function,
                node_idx,
                Visibility::Package,
            );
            r.node_symbols.insert(children[0], symbol);
        }
        NodeKind::StructDef => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            let type_idx = r.register_type(TypeKind::Struct {
                name: name_str,
                fields: Vec::new(),
            });
            let sym_id = r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package);
            r.symbol_mut(sym_id).type_index = type_idx;
        }
        NodeKind::EnumDef => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            let type_idx = r.register_type(TypeKind::Enum {
                name: name_str,
                variants: Vec::new(),
            });
            let sym_id = r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package);
            r.symbol_mut(sym_id).type_index = type_idx;
        }
        NodeKind::Newtype => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            let type_idx = r.register_type(TypeKind::Newtype {
                name: name_str,
                inner: type_pool::Intrinsic::Unit.type_index(),
            });
            r.type_pool.get_mut(type_idx).size = 8;
            r.type_pool.get_mut(type_idx).align = 8;
            let sym_id = r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package);
            r.symbol_mut(sym_id).type_index = type_idx;
        }
        NodeKind::TraitDef => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            let type_idx = r.register_type(TypeKind::Trait {
                name: name_str,
                parents: Vec::new(),
                assoc_types: Vec::new(),
            });
            let sym_id =
                r.define_symbol(name_str, SymbolKind::Trait, node_idx, Visibility::Package);
            r.symbol_mut(sym_id).type_index = type_idx;
        }
        NodeKind::EffectDef | NodeKind::AsyncEffectDef => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            let is_async = kind == NodeKind::AsyncEffectDef;
            let type_idx = r.register_type(TypeKind::Effect {
                params: Vec::new(),
                ret: type_pool::TypeIndex::INVALID,
                is_async,
            });
            let sym_id =
                r.define_symbol(name_str, SymbolKind::Effect, node_idx, Visibility::Package);
            r.symbol_mut(sym_id).type_index = type_idx;
        }
        NodeKind::ModuleDef => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            let type_idx = r.register_type(TypeKind::Module { name: name_str });
            let sym_id = if r.options.detached_package_roots.contains(&node_idx) {
                r.alloc_unbound_symbol(name_str, SymbolKind::Module, node_idx)
            } else {
                r.define_symbol(name_str, SymbolKind::Module, node_idx, Visibility::Package)
            };
            r.symbol_mut(sym_id).type_index = type_idx;
            r.node_symbols.insert(children[0], sym_id);
            r.node_symbols.insert(node_idx, sym_id);
        }
        NodeKind::Typealias => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            // Target filled in during type resolution / resolve_typealias.
            let _ = r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package);
        }
        NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl => {
            let children = ast.fixed_children(node_idx);
            let name = children[0];
            let has_else = children.len() > 3 && !children[3].is_null();
            let sym_kind = if kind == NodeKind::ConstDecl {
                SymbolKind::Constant
            } else {
                SymbolKind::Variable
            };
            if ast.node(name).kind == NodeKind::Id && !has_else {
                r.define_symbol(
                    ast.node(name).str_id,
                    sym_kind,
                    node_idx,
                    Visibility::Package,
                );
            } else {
                let mut leaves = Vec::new();
                if !has_else && collect_module_binding_leaves(ast, name, &mut leaves) {
                    let mut seen_in_decl = std::collections::HashSet::new();
                    for leaf in leaves {
                        let leaf_name = ast.node(leaf).str_id;
                        if !seen_in_decl.insert(leaf_name) {
                            r.diag_ctx
                                .error("pattern binds the same name more than once".into())
                                .with_primary_span(ast.node(leaf).span)
                                .emit(r.diag_ctx);
                            continue;
                        }
                        if !r.options.detached_package_roots.contains(&node_idx)
                            && let Some(existing) = r.lookup_current_scope(leaf_name)
                        {
                            if r.symbols[existing.0 as usize].def_node == node_idx {
                                r.node_symbols.insert(leaf, existing);
                            } else {
                                r.diag_ctx
                                    .error(format!(
                                        "duplicate definition `{}`",
                                        str_interner::get(leaf_name)
                                    ))
                                    .with_primary_span(ast.node(leaf).span)
                                    .emit(r.diag_ctx);
                            }
                            continue;
                        }
                        let sym =
                            r.define_symbol(leaf_name, sym_kind, node_idx, Visibility::Package);
                        if !r.options.detached_package_roots.contains(&node_idx) {
                            r.node_symbols.insert(leaf, sym);
                        }
                    }
                } else {
                    r.diag_ctx
                        .error("module value destructuring is not implemented".into())
                        .with_primary_span(ast.node(name).span)
                        .emit(r.diag_ctx);
                }
            }
        }
        NodeKind::StructField | NodeKind::EnumVariant => {
            let name = ast.fixed_children(node_idx)[0];
            r.define_symbol(
                ast.node(name).str_id,
                if kind == NodeKind::StructField {
                    SymbolKind::Field
                } else {
                    SymbolKind::EnumVariant
                },
                node_idx,
                Visibility::Package,
            );
        }
        NodeKind::PubDef | NodeKind::PrivateDef => {
            // Unwrap visibility wrapper and forward-declare the inner definition.
            let children = ast.fixed_children(node_idx);
            if !children.is_empty() {
                forward_declare_one(r, ast, children[0]);
                let visibility = if kind == NodeKind::PrivateDef {
                    Visibility::Private
                } else {
                    Visibility::Public
                };
                if let Some(&symbol) = r.node_symbols.get(&children[0]) {
                    r.symbol_mut(symbol).visibility = visibility;
                }
                let mut target_decl = children[0];
                while !target_decl.is_null()
                    && matches!(
                        ast.node(target_decl).kind,
                        NodeKind::PubDef | NodeKind::PrivateDef | NodeKind::GlobalDecl
                    )
                {
                    target_decl = ast.fixed_children(target_decl)[0];
                }
                if !target_decl.is_null() {
                    let current = r.current_scope;
                    for sym in &mut r.symbols {
                        if sym.scope == current && sym.def_node == target_decl {
                            sym.visibility = visibility;
                        }
                    }
                }
            }
        }
        NodeKind::GlobalDecl | NodeKind::AssocDecl => {
            let inner = ast.fixed_children(node_idx)[0];
            forward_declare_one(r, ast, inner);
            if let Some(&symbol) = r.node_symbols.get(&inner) {
                r.node_symbols.insert(node_idx, symbol);
            }
        }
        _ => {}
    }
    if matches!(
        kind,
        NodeKind::FunctionDef
            | NodeKind::StructDef
            | NodeKind::EnumDef
            | NodeKind::Newtype
            | NodeKind::TraitDef
            | NodeKind::EffectDef
            | NodeKind::AsyncEffectDef
            | NodeKind::ModuleDef
            | NodeKind::Typealias
            | NodeKind::ConstDecl
            | NodeKind::LetDecl
            | NodeKind::VarDecl
            | NodeKind::StructField
            | NodeKind::EnumVariant
            | NodeKind::AssocBinding
    ) {
        let name = ast.fixed_children(node_idx)[0];
        if !r.options.detached_package_roots.contains(&node_idx)
            && ast.node(name).kind == NodeKind::Id
            && let Some(symbol) = r.lookup_current_scope(ast.node(name).str_id)
        {
            r.node_symbols.insert(name, symbol);
            r.node_symbols.insert(node_idx, symbol);
        }
    }
}

// ---------------------------------------------------------------------------
// Identifier resolution
// ---------------------------------------------------------------------------

fn resolve_id(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex, str_id: str_interner::StrId) {
    match r.lookup_with_visibility(str_id) {
        Ok(Some(sym_id)) => {
            r.node_symbols.insert(node_idx, sym_id);
        }
        Err(_) => {
            r.diag_ctx
                .error(format!(
                    "`{}` is not visible from this module",
                    str_interner::get(str_id)
                ))
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
        }
        Ok(None) => {
            let name = str_interner::get(str_id);
            let span = ast.node(node_idx).span;
            r.diag_ctx
                .error(format!("undefined name `{name}`"))
                .with_primary_span(span)
                .with_label(
                    span,
                    format!("`{name}` is not defined in this scope"),
                    Level::Error,
                )
                .emit(r.diag_ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// Variable declarations
// ---------------------------------------------------------------------------

/// `let pat: Type = value else { ... }` / `const pat: Type = value else { ... }`
fn resolve_let_or_const(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex, kind: NodeKind) {
    let children = ast.fixed_children(node_idx);
    // [0] pattern  [1] type  [2] value  [3] else
    // Resolve value, type, and else branch BEFORE binding the pattern.
    if children.len() > 2 {
        resolve_names(r, ast, children[2]);
    }
    if children.len() > 1 {
        resolve_names(r, ast, children[1]);
    }
    let has_else = children.len() > 3 && !children[3].is_null();
    if has_else {
        if ast.node(children[3]).kind == NodeKind::Block {
            resolve_names(r, ast, children[3]);
        } else {
            r.push_scope(node_idx, None);
            resolve_names(r, ast, children[3]);
            r.pop_scope();
        }
    }
    resolve_pattern_with_mode(r, ast, children[0], kind, has_else);
    if let Some(&symbol) = r.node_symbols.get(&children[0]) {
        r.node_symbols.insert(node_idx, symbol);
    }

    // If `const name = .foo'builtin`, promote the binding to BuiltinFunction
    // so calls lower to CallBuiltin.
    if !has_else && kind == NodeKind::ConstDecl && children.len() > 2 {
        let pat = children[0];
        let rhs = children[2];
        if !pat.is_null()
            && !rhs.is_null()
            && ast.node(pat).kind == NodeKind::Id
            && let Some(&rhs_sym) = r.node_symbols.get(&rhs)
            && let Some(&builtin_id) = r.builtin_fns.get(&rhs_sym)
            && let Some(&pat_sym) = r.node_symbols.get(&pat)
        {
            r.symbol_mut(pat_sym).kind = SymbolKind::BuiltinFunction(builtin_id);
            r.builtin_fns.insert(pat_sym, builtin_id);
        }
    }
}

/// `var pat: Type = value else { ... }`
fn resolve_var_decl(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] pattern  [1] type  [2] value  [3] else
    if children.len() > 2 {
        resolve_names(r, ast, children[2]);
    }
    if children.len() > 1 {
        resolve_names(r, ast, children[1]);
    }
    let has_else = children.len() > 3 && !children[3].is_null();
    if has_else {
        if ast.node(children[3]).kind == NodeKind::Block {
            resolve_names(r, ast, children[3]);
        } else {
            r.push_scope(node_idx, None);
            resolve_names(r, ast, children[3]);
            r.pop_scope();
        }
    }
    resolve_pattern_with_mode(r, ast, children[0], NodeKind::VarDecl, has_else);
    if let Some(&symbol) = r.node_symbols.get(&children[0]) {
        r.node_symbols.insert(node_idx, symbol);
    }
}

fn resolve_global_decl(r: &mut Resolver, ast: &Ast, node: NodeIndex) {
    let scope_node = r.scopes[r.current_scope.0 as usize].node;
    if scope_node.is_null()
        || !matches!(
            ast.node(scope_node).kind,
            NodeKind::FileScope
                | NodeKind::ModuleDef
                | NodeKind::StructDef
                | NodeKind::EnumDef
                | NodeKind::ImplDef
                | NodeKind::ImplTraitDef
                | NodeKind::ExtendDef
                | NodeKind::ExtendTraitDef
        )
    {
        r.diag_ctx
            .error("global declarations are only allowed in file, module or type scopes".into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
        return;
    }
    let inner = ast.fixed_children(node)[0];
    resolve_names(r, ast, inner);
    if let Some(&symbol) = r.node_symbols.get(&inner) {
        r.node_symbols.insert(node, symbol);
    }
}

// ---------------------------------------------------------------------------
// Function definition
// ---------------------------------------------------------------------------

fn resolve_function_def(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] ret_type  [2] body  [3] capability
    // multi: params

    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;

    if str_interner::get(name_str) == "__init__" && !ast.multi_children(node_idx).is_empty() {
        r.diag_ctx
            .error("__init__ must have no parameters".into())
            .with_primary_span(ast.node(node_idx).span)
            .emit(r.diag_ctx);
    }

    // Retain the declaration identity even if a preceding local value shadows
    // its forward binding. The declaration reintroduces that function name at
    // its source position; subsequent ordinary values can shadow it again.
    let sym_id = if let Some(&symbol) = r.node_symbols.get(&name_node) {
        symbol
    } else if let Some(existing) = r.lookup_current_scope(name_str) {
        existing
    } else {
        r.define_symbol(
            name_str,
            SymbolKind::Function,
            node_idx,
            Visibility::Package,
        )
    };
    r.scopes[r.current_scope.0 as usize]
        .bindings
        .insert(name_str, sym_id);
    r.node_symbols.insert(name_node, sym_id);

    // Function body gets its own scope.
    r.push_scope(node_idx, None);

    // Bind parameters.
    for &param in ast.multi_children(node_idx) {
        resolve_param(r, ast, param);
    }
    crate::arguments::resolve_default_names(r, ast, node_idx);
    // Resolve return type.
    resolve_names(r, ast, children[1]);
    // Resolve body.
    resolve_names(r, ast, children[2]);
    // Resolve capability / handles expr.
    resolve_names(r, ast, children[3]);

    r.pop_scope();
}

// ---------------------------------------------------------------------------
// Struct / Enum definitions
// ---------------------------------------------------------------------------

fn resolve_struct_def(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name   multi: fields + body items
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;

    // If not already forward-declared, define now.
    let (sym_id, type_idx) = if let Some(existing) = r.lookup_current_scope(name_str) {
        let ti = r.symbols[existing.0 as usize].type_index;
        (existing, ti)
    } else {
        let type_idx = r.register_type(TypeKind::Struct {
            name: name_str,
            fields: Vec::new(),
        });
        let sym_id = r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package);
        r.symbol_mut(sym_id).type_index = type_idx;
        (sym_id, type_idx)
    };
    r.node_symbols.insert(name_node, sym_id);

    // Associated scope.
    let original = r.current_scope;
    if let Some(&scope) = r.imports.module_scopes.get(&node_idx) {
        r.current_scope = scope;
    } else {
        r.push_scope(node_idx, Some(type_idx));
        forward_declare_scope(r, ast, node_idx);
    }
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.current_scope = original;
}

fn resolve_struct_field(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] type  [2] default
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;
    let sym_id = r.lookup_current_scope(name_str).unwrap_or_else(|| {
        r.define_symbol(name_str, SymbolKind::Field, node_idx, Visibility::Package)
    });
    r.node_symbols.insert(name_node, sym_id);

    resolve_names(r, ast, children[1]); // type
    resolve_names(r, ast, children[2]); // default value
    crate::structs::validate_default_names(r, ast, children[2]);
}

fn resolve_enum_def(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name   multi: variants + body items
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;

    // If not already forward-declared, define now.
    let (sym_id, type_idx) = if let Some(existing) = r.lookup_current_scope(name_str) {
        let ti = r.symbols[existing.0 as usize].type_index;
        (existing, ti)
    } else {
        let type_idx = r.register_type(TypeKind::Enum {
            name: name_str,
            variants: Vec::new(),
        });
        let sym_id = r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package);
        r.symbol_mut(sym_id).type_index = type_idx;
        (sym_id, type_idx)
    };
    r.node_symbols.insert(name_node, sym_id);

    let original = r.current_scope;
    if let Some(&scope) = r.imports.module_scopes.get(&node_idx) {
        r.current_scope = scope;
    } else {
        r.push_scope(node_idx, Some(type_idx));
        forward_declare_scope(r, ast, node_idx);
    }
    let mut variant_index = 0u32;
    for &wrapped in ast.multi_children(node_idx) {
        let member = crate::structs::unwrap_member(ast, wrapped);
        if ast.node(member).kind == NodeKind::EnumVariant {
            resolve_enum_variant(r, ast, member, variant_index);
            variant_index += 1;
        } else {
            resolve_names(r, ast, member);
        }
    }
    r.current_scope = original;
}

fn resolve_enum_variant(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex, variant_index: u32) {
    let children = ast.fixed_children(node_idx);
    // [0] name   multi: params (variant fields)
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;
    let sym_id = r.lookup_current_scope(name_str).unwrap_or_else(|| {
        r.define_symbol(
            name_str,
            SymbolKind::EnumVariant,
            node_idx,
            Visibility::Package,
        )
    });
    r.node_symbols.insert(name_node, sym_id);
    r.enum_variant_indices
        .entry(sym_id)
        .or_insert(variant_index);

    if let Some(ty) = r.scopes[r.current_scope.0 as usize].assoc_type {
        r.symbol_mut(sym_id).type_index = ty;
    }
    r.push_scope(node_idx, None);
    for &field in ast.multi_children(node_idx) {
        resolve_param(r, ast, field);
    }
    crate::arguments::resolve_default_names(r, ast, node_idx);
    r.pop_scope();
}

// ---------------------------------------------------------------------------
// Trait definition
// ---------------------------------------------------------------------------

fn resolve_trait_def(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] parents (ListOf or NULL)  multi: members
    let name_node = children[0];
    let parents_node = children[1];
    let name_str = ast.node(name_node).str_id;

    let (sym_id, type_idx) = if let Some(existing) = r.lookup_current_scope(name_str) {
        let ti = r.symbols[existing.0 as usize].type_index;
        (existing, ti)
    } else {
        let type_idx = r.register_type(TypeKind::Trait {
            name: name_str,
            parents: Vec::new(),
            assoc_types: Vec::new(),
        });
        let sym_id = r.define_symbol(name_str, SymbolKind::Trait, node_idx, Visibility::Package);
        r.symbol_mut(sym_id).type_index = type_idx;
        (sym_id, type_idx)
    };
    r.node_symbols.insert(name_node, sym_id);

    // Resolve parent trait expressions before member names need inherited binders.
    if !parents_node.is_null() {
        resolve_names(r, ast, parents_node);
        crate::traits::prepare_trait_parents(r, ast);
    }

    r.push_scope(node_idx, Some(type_idx));
    forward_declare_scope(r, ast, node_idx);
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.pop_scope();
}

fn resolve_trait_fn(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex, kind: NodeKind) {
    let children = ast.fixed_children(node_idx);
    // TraitDefFn:   [0] name  [1] ret_type  [2] capability       multi: params
    // TraitDeriveFn:[0] name  [1] ret_type  [2] body  [3] cap   multi: params
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;
    let sym_id = r.define_symbol(
        name_str,
        SymbolKind::Function,
        node_idx,
        Visibility::Package,
    );
    r.node_symbols.insert(name_node, sym_id);

    if kind == NodeKind::TraitDeriveFn {
        r.push_scope(node_idx, None);
        for &param in ast.multi_children(node_idx) {
            resolve_param(r, ast, param);
        }
        resolve_names(r, ast, children[1]); // ret_type
        resolve_names(r, ast, children[2]); // body
        resolve_names(r, ast, children[3]); // capability
        r.pop_scope();
    } else {
        // TraitDefFn — no body, just params and return type.
        r.push_scope(node_idx, None);
        for &param in ast.multi_children(node_idx) {
            resolve_param(r, ast, param);
        }
        resolve_names(r, ast, children[1]); // ret_type
        resolve_names(r, ast, children[2]); // capability
        r.pop_scope();
    }
}

// ---------------------------------------------------------------------------
// Effect definition
// ---------------------------------------------------------------------------

fn resolve_effect_def(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex, kind: NodeKind) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] ret_type   multi: params
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;
    let is_async = kind == NodeKind::AsyncEffectDef;

    let (sym_id, type_idx) = if let Some(existing) = r.lookup_current_scope(name_str) {
        let ti = r.symbols[existing.0 as usize].type_index;
        (existing, ti)
    } else {
        let type_idx = r.register_type(TypeKind::Effect {
            params: Vec::new(),
            ret: type_pool::TypeIndex::INVALID,
            is_async,
        });
        let sym_id = r.define_symbol(name_str, SymbolKind::Effect, node_idx, Visibility::Package);
        r.symbol_mut(sym_id).type_index = type_idx;
        (sym_id, type_idx)
    };
    r.node_symbols.insert(name_node, sym_id);

    // Effect params live in their own scope (for type resolution).
    r.push_scope(node_idx, Some(type_idx));
    for &param in ast.multi_children(node_idx) {
        resolve_param(r, ast, param);
    }
    crate::arguments::resolve_default_names(r, ast, node_idx);
    resolve_names(r, ast, children[1]); // ret_type
    r.pop_scope();
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

fn resolve_module_def(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name   multi: members
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;

    let (sym_id, type_idx) = if let Some(existing) = r.node_symbols.get(&name_node).copied() {
        let ti = r.symbols[existing.0 as usize].type_index;
        (existing, ti)
    } else {
        let type_idx = r.register_type(TypeKind::Module { name: name_str });
        let sym_id = r.define_symbol(name_str, SymbolKind::Module, node_idx, Visibility::Package);
        r.symbol_mut(sym_id).type_index = type_idx;
        (sym_id, type_idx)
    };
    r.node_symbols.insert(name_node, sym_id);

    let original = r.current_scope;
    if let Some(&scope) = r.imports.module_scopes.get(&node_idx) {
        r.current_scope = scope;
    } else {
        r.push_scope(node_idx, Some(type_idx));
        forward_declare_scope(r, ast, node_idx);
    }
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.current_scope = original;
}

// ---------------------------------------------------------------------------
// Impl / Extend blocks
// ---------------------------------------------------------------------------

fn resolve_impl(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    // ImplDef:      [0] type_expr            multi: members
    // ImplTraitDef: [0] trait_expr [1] type   multi: members

    // Resolve type exprs first so we can gate impls on builtin types.
    for &child in ast.fixed_children(node_idx) {
        resolve_names(r, ast, child);
    }

    let implementor_node = match ast.node(node_idx).kind {
        NodeKind::ImplDef => ast.fixed_children(node_idx)[0],
        NodeKind::ImplTraitDef => ast.fixed_children(node_idx)[1],
        _ => NodeIndex::NULL,
    };
    check_builtin_impl_allowed(r, ast, node_idx, implementor_node);

    let original = r.current_scope;
    if let Some(&scope) = r.imports.module_scopes.get(&node_idx) {
        r.current_scope = scope;
        if r.scopes[scope.0 as usize].assoc_type.is_none() {
            r.diag_ctx
                .error("impl requires a statically known type declaration".into())
                .with_primary_span(ast.node(implementor_node).span)
                .emit(r.diag_ctx);
        }
    } else {
        let target =
            type_index_of_expr(r, implementor_node).and_then(|ty| r.type_pool.canonical_type(ty));
        r.push_scope(node_idx, target);
        forward_declare_scope(r, ast, node_idx);
    }
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.current_scope = original;
}

fn resolve_extend(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    // Same structure as impl.
    for &child in ast.fixed_children(node_idx) {
        resolve_names(r, ast, child);
    }
    let implementor_node = match ast.node(node_idx).kind {
        NodeKind::ExtendDef => ast.fixed_children(node_idx)[0],
        NodeKind::ExtendTraitDef => ast.fixed_children(node_idx)[1],
        _ => NodeIndex::NULL,
    };
    check_builtin_impl_allowed(r, ast, node_idx, implementor_node);

    let original = r.current_scope;
    if !r.imports.module_scopes.contains_key(&node_idx) {
        prepare_scope_tree(r, ast, node_idx, None);
    }
    crate::associated::attach(r, ast);
    r.current_scope = r.imports.module_scopes[&node_idx];
    if r.scopes[r.current_scope.0 as usize].assoc_type.is_none() {
        r.diag_ctx
            .error("extend requires a statically known type declaration".into())
            .with_primary_span(ast.node(implementor_node).span)
            .emit(r.diag_ctx);
    }
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.current_scope = original;
}

/// Only privileged packages may `impl` / `extend` builtin (intrinsic) types.
fn check_builtin_impl_allowed(
    r: &mut Resolver,
    ast: &Ast,
    impl_node: NodeIndex,
    type_node: NodeIndex,
) {
    if r.options.builtin_access
        || r.options.privileged_nodes.contains(&impl_node)
        || type_node.is_null()
    {
        return;
    }
    let Some(ti) = type_index_of_expr(r, type_node) else {
        return;
    };
    if r.type_pool.as_intrinsic(ti).is_some() || r.type_pool.is_reserved_collection_role(ti) {
        let span = ast.node(impl_node).span;
        r.diag_ctx
            .error("cannot implement methods on a builtin type outside a privileged package (std/core/alloc)".to_string())
            .with_primary_span(span)
            .emit(r.diag_ctx);
    }
}

fn type_index_of_expr(r: &Resolver, node: NodeIndex) -> Option<type_pool::TypeIndex> {
    if let Some(&sym_id) = r.node_symbols.get(&node) {
        let sym = &r.symbols[sym_id.0 as usize];
        if sym.type_index != type_pool::TypeIndex::INVALID {
            return Some(sym.type_index);
        }
    }
    None
}

/// `typealias Name = type_expr`
fn resolve_typealias(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] type expr
    if children.len() < 2 {
        return;
    }
    let name_node = children[0];
    let type_expr = children[1];
    resolve_names(r, ast, type_expr);

    let name_str = ast.node(name_node).str_id;
    let sym_id = if let Some(existing) = r.lookup_current_scope(name_str) {
        existing
    } else {
        r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package)
    };
    r.node_symbols.insert(name_node, sym_id);
    r.node_symbols.insert(node_idx, sym_id);

    // Resolve target type now if possible (View / Id already bound).
    if let Some(factory) = crate::type_factories::identity(r, type_expr) {
        r.symbol_mut(sym_id).kind = SymbolKind::TypeFactory(factory);
        return;
    }
    if let Some(target) = resolve_typealias_target(r, ast, type_expr) {
        let alias_ti = r.register_type(TypeKind::Typealias {
            name: name_str,
            target,
        });
        r.symbol_mut(sym_id).type_index = alias_ti;
    }
}

/// `newtype Name = type_expr`
fn resolve_newtype(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    if children.len() < 2 {
        return;
    }
    let name_node = children[0];
    let type_expr = children[1];
    resolve_names(r, ast, type_expr);

    let name_str = ast.node(name_node).str_id;
    let sym_id = if let Some(existing) = r.lookup_current_scope(name_str) {
        existing
    } else {
        let type_idx = r.register_type(TypeKind::Newtype {
            name: name_str,
            inner: type_pool::Intrinsic::Unit.type_index(),
        });
        r.type_pool.get_mut(type_idx).size = 8;
        r.type_pool.get_mut(type_idx).align = 8;
        let sym_id = r.define_symbol(name_str, SymbolKind::Type, node_idx, Visibility::Package);
        r.symbol_mut(sym_id).type_index = type_idx;
        sym_id
    };
    r.node_symbols.insert(name_node, sym_id);
    r.node_symbols.insert(node_idx, sym_id);
}

fn resolve_typealias_target(
    r: &mut Resolver,
    ast: &Ast,
    type_expr: NodeIndex,
) -> Option<type_pool::TypeIndex> {
    if type_expr.is_null() {
        return None;
    }
    if ast.node(type_expr).kind == NodeKind::Call {
        return crate::typing::resolve_type_expr_inner(r, ast, type_expr);
    }
    // Prefer symbol binding from View / Id.
    if let Some(&sym_id) = r.node_symbols.get(&type_expr) {
        let sym = &r.symbols[sym_id.0 as usize];
        if sym.kind == SymbolKind::Type && sym.type_index != type_pool::TypeIndex::INVALID {
            return Some(sym.type_index);
        }
    }
    // Fallback: named Id lookup.
    if ast.node(type_expr).kind == NodeKind::Id
        && let Some(&sym_id) = r.node_symbols.get(&type_expr)
    {
        let ti = r.symbols[sym_id.0 as usize].type_index;
        if ti != type_pool::TypeIndex::INVALID {
            return Some(ti);
        }
    }
    let _ = ast;
    None
}

/// `expr ' view_name` — currently only `'builtin` is handled.
fn resolve_view(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    if children.len() < 2 {
        return;
    }
    let object = children[0];
    let view_id_node = children[1];
    let view_name = str_interner::get(ast.node(view_id_node).str_id);
    if view_name == "builtin" && !object.is_null() && ast.node(object).kind == NodeKind::Symbol {
        r.node_scopes.insert(object, r.current_scope);
        if let Some(&id_node) = ast.fixed_children(object).first()
            && !id_node.is_null()
        {
            r.node_scopes.insert(id_node, r.current_scope);
        }
    } else {
        resolve_names(r, ast, object);
    }

    if view_name != "builtin" {
        // Other views (`'type`, …) are not resolved here yet.
        return;
    }

    let is_privileged = r.options.builtin_access || r.options.privileged_nodes.contains(&node_idx);
    let has_ffi_node = r.options.ffi_nodes.contains(&node_idx);

    if !is_privileged && !has_ffi_node {
        if !object.is_null()
            && ast.node(object).kind == NodeKind::Symbol
            && let Some(&sym_id_node) = ast.fixed_children(object).first()
            && !sym_id_node.is_null()
        {
            let symbol_name = str_interner::get(ast.node(sym_id_node).str_id);
            if is_ffi_builtin_name(&symbol_name) {
                let span = ast.node(node_idx).span;
                r.diag_ctx
                    .error("FFI builtin requires `[ffi] allow = true` in package.toml".to_string())
                    .with_primary_span(span)
                    .emit(r.diag_ctx);
                return;
            }
        }
        let span = ast.node(node_idx).span;
        r.diag_ctx
            .error(
                "`\'builtin` view is only allowed in privileged packages (std/core/alloc)"
                    .to_string(),
            )
            .with_primary_span(span)
            .emit(r.diag_ctx);
        return;
    }

    if object.is_null() || ast.node(object).kind != NodeKind::Symbol {
        let span = ast.node(node_idx).span;
        r.diag_ctx
            .error(
                "`\'builtin` requires a symbol literal on the left (e.g. `.print\'builtin`)"
                    .to_string(),
            )
            .with_primary_span(span)
            .emit(r.diag_ctx);
        return;
    }

    // Symbol: [0] id — name lives on the child Id node.
    let sym_children = ast.fixed_children(object);
    if sym_children.is_empty() || sym_children[0].is_null() {
        return;
    }
    let symbol_name = str_interner::get(ast.node(sym_children[0]).str_id);
    if !is_privileged {
        if !is_ffi_builtin_name(&symbol_name) {
            let span = ast.node(node_idx).span;
            r.diag_ctx
                .error(
                    "`\'builtin` view is only allowed in privileged packages (std/core/alloc)"
                        .to_string(),
                )
                .with_primary_span(span)
                .emit(r.diag_ctx);
            return;
        }
        if symbol_name == "__ffi_dlopen"
            && let Some(allowlist) = r.options.ffi_library_allowlists.get(&node_idx)
        {
            r.restricted_dlopen_views
                .insert(node_idx, allowlist.clone());
        }
    }
    match r.resolve_builtin_view(node_idx, &symbol_name) {
        Some(sym_id) => {
            r.node_symbols.insert(node_idx, sym_id);
        }
        None => {
            let span = ast.node(node_idx).span;
            r.diag_ctx
                .error(format!("unknown builtin `{symbol_name}`"))
                .with_primary_span(span)
                .emit(r.diag_ctx);
        }
    }
}

fn is_ffi_builtin_name(symbol_name: &str) -> bool {
    matches!(
        symbol_name,
        "__ffi_dlopen" | "__ffi_dlsym" | "__ffi_call" | "__ffi_call4" | "__ffi_call6"
    )
}

// ---------------------------------------------------------------------------
// Pub wrapper
// ---------------------------------------------------------------------------

fn resolve_pub_def(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] = inner definition
    if children.is_empty() {
        return;
    }
    let inner = children[0];
    if inner.is_null() {
        return;
    }

    // Resolve the inner definition first — it will define symbols at Package
    // visibility.  Afterwards we upgrade the outermost symbol to Public.
    if ast.node(inner).kind == NodeKind::UseStatement {
        crate::imports::resolve_local_use(r, ast, inner, Visibility::Public);
        return;
    }
    resolve_names(r, ast, inner);
    let name = ast.fixed_children(inner).first().copied();
    if let Some(symbol) = r
        .node_symbols
        .get(&inner)
        .copied()
        .or_else(|| name.and_then(|name| r.node_symbols.get(&name).copied()))
    {
        r.symbol_mut(symbol).visibility = if ast.node(node_idx).kind == NodeKind::PrivateDef {
            Visibility::Private
        } else {
            Visibility::Public
        };
    }
}

// ---------------------------------------------------------------------------
// Lambda
// ---------------------------------------------------------------------------

fn resolve_lambda(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] body  [1] return_type   multi: params
    r.push_scope(node_idx, None);
    for &param in ast.multi_children(node_idx) {
        resolve_param(r, ast, param);
    }
    crate::arguments::resolve_default_names(r, ast, node_idx);
    resolve_names(r, ast, children[1]); // return type
    resolve_names(r, ast, children[0]); // body
    r.pop_scope();
}

fn resolve_case_closure(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    r.push_scope(node_idx, None);
    let mut arms = Vec::new();
    crate::flatten_case_arms(ast, node_idx, &mut arms);
    for arm in arms {
        let previous = r.current_scope;
        r.push_scope(arm, None);
        r.node_scopes.insert(arm, r.current_scope);
        let children = ast.fixed_children(arm);
        resolve_pattern(r, ast, children[0], NodeKind::CaseArm);
        resolve_names(r, ast, children[1]);
        r.current_scope = previous;
    }
    r.pop_scope();
}

fn resolve_defer(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    r.push_scope(node_idx, None);
    resolve_names(r, ast, ast.fixed_children(node_idx)[0]);
    r.pop_scope();
}

// ---------------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------------

fn validate_control_placement(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) {
    let kind = ast.node(node).kind;
    let keyword = match kind {
        NodeKind::OptionPropagation => "?",
        NodeKind::ErrorPropagation => "!",
        NodeKind::ReturnStatement => "return",
        NodeKind::ResumeStatement => "resume",
        NodeKind::BreakStatement => "break",
        _ => "continue",
    };
    let is_loop_control = matches!(kind, NodeKind::BreakStatement | NodeKind::ContinueStatement);
    let label = is_loop_control
        .then(|| ast.fixed_children(node)[0])
        .filter(|node| !node.is_null())
        .map(|node| ast.node(node).str_id);
    let mut scope = Some(r.current_scope);
    let mut allowed = false;
    while let Some(id) = scope {
        let entry = &r.scopes[id.0 as usize];
        if !entry.node.is_null() {
            let owner = ast.node(entry.node).kind;
            if owner == NodeKind::DeferStatement {
                r.diag_ctx
                    .error(format!("`{keyword}` cannot exit a `defer` block"))
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
                return;
            }
            if matches!(
                owner,
                NodeKind::FunctionDef
                    | NodeKind::TraitDeriveFn
                    | NodeKind::Lambda
                    | NodeKind::CaseMap
                    | NodeKind::CaseAlternative
                    | NodeKind::HandlesStatement
            ) || (owner == NodeKind::CaseArm && r.handler_arms.contains(&entry.node))
            {
                allowed = !is_loop_control;
                break;
            }
            if is_loop_control && matches!(owner, NodeKind::WhileLoop | NodeKind::ForLoop) {
                let target_label = ast.fixed_children(entry.node)[0];
                if label.is_none()
                    || (!target_label.is_null() && Some(ast.node(target_label).str_id) == label)
                {
                    allowed = true;
                    break;
                }
            }
            if matches!(
                owner,
                NodeKind::FileScope | NodeKind::ModuleDef | NodeKind::StructDef | NodeKind::EnumDef
            ) {
                break;
            }
        }
        scope = entry.parent;
    }
    if !allowed {
        let context = if is_loop_control {
            "a loop body"
        } else {
            "a function or handler body"
        };
        r.diag_ctx
            .error(format!("`{keyword}` is only allowed inside {context}"))
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
}

fn resolve_for_loop(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] label  [1] pattern  [2] iterable  [3] body
    r.push_scope(node_idx, None);
    // Resolve the iterable before binding the pattern.
    resolve_names(r, ast, children[2]);
    resolve_pattern(r, ast, children[1], NodeKind::ForLoop);
    // Label.
    if !children[0].is_null() {
        let label_str = ast.node(children[0]).str_id;
        let sym_id = r.define_symbol(
            label_str,
            SymbolKind::Label,
            children[0],
            Visibility::Private,
        );
        r.node_symbols.insert(children[0], sym_id);
    }
    resolve_names(r, ast, children[3]); // body
    r.pop_scope();
}

fn resolve_while_loop(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] label  [1] condition  [2] body
    r.push_scope(node_idx, None);
    if !children[0].is_null() {
        let label_str = ast.node(children[0]).str_id;
        let sym_id = r.define_symbol(
            label_str,
            SymbolKind::Label,
            children[0],
            Visibility::Private,
        );
        r.node_symbols.insert(children[0], sym_id);
    }
    resolve_names(r, ast, children[1]); // condition
    resolve_names(r, ast, children[2]); // body
    r.pop_scope();
}

fn resolve_if(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] condition  [1] then  [2] else
    resolve_names(r, ast, children[0]);
    resolve_names(r, ast, children[1]);
    resolve_names(r, ast, children[2]);
}

fn resolve_when(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let arms = ast.multi_children(node_idx);
    let mut seen_else = false;
    for (idx, &arm) in arms.iter().enumerate() {
        if arm.is_null() {
            continue;
        }
        r.node_scopes.insert(arm, r.current_scope);
        match ast.node(arm).kind {
            NodeKind::ConditionArm => {
                let children = ast.fixed_children(arm);
                resolve_names(r, ast, children[0]);
                let body = children[1];
                if !body.is_null() && ast.node(body).kind != NodeKind::Block {
                    r.push_scope(arm, None);
                    resolve_names(r, ast, body);
                    r.pop_scope();
                } else {
                    resolve_names(r, ast, body);
                }
            }
            NodeKind::ElseConditionArm => {
                if seen_else {
                    r.diag_ctx
                        .error("at most one `else` arm is allowed in `when`".into())
                        .with_primary_span(ast.node(arm).span)
                        .emit(r.diag_ctx);
                } else if idx + 1 != arms.len() {
                    r.diag_ctx
                        .error("the `else` arm in `when` must be the last arm".into())
                        .with_primary_span(ast.node(arm).span)
                        .emit(r.diag_ctx);
                }
                seen_else = true;
                let body = ast.fixed_children(arm)[0];
                if !body.is_null() && ast.node(body).kind != NodeKind::Block {
                    r.push_scope(arm, None);
                    resolve_names(r, ast, body);
                    r.pop_scope();
                } else {
                    resolve_names(r, ast, body);
                }
            }
            _ => resolve_names(r, ast, arm),
        }
    }
}

fn infix_fn_name(r: &Resolver<'_>, ast: &Ast, node_idx: NodeIndex) -> Option<str_interner::StrId> {
    let direct = ast.node(node_idx).str_id;
    if direct != str_interner::StrId::from_raw(0) {
        return Some(direct);
    }
    let children = ast.fixed_children(node_idx);
    if children.len() < 2 || children[0].is_null() || children[1].is_null() {
        return None;
    }
    let lhs_span = ast.node(children[0]).span;
    let rhs_span = ast.node(children[1]).span;
    if lhs_span.hi() >= rhs_span.lo() {
        return None;
    }
    let between = rustc_span::Span::new(lhs_span.hi(), rhs_span.lo());
    let snippet = r.diag_ctx.source_map().span_to_snippet(between).ok()?;
    let (tokens, _) = lexer::tokenize(&snippet);
    let tok = tokens
        .into_iter()
        .find(|t| t.kind == lexer::TokenKind::Id)?;
    let slice = snippet.get(tok.from as usize..tok.to as usize)?;
    Some(str_interner::intern(slice))
}

fn resolve_infix_fn_call(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    resolve_names(r, ast, children[0]);
    resolve_names(r, ast, children[1]);
    let Some(fn_name) = infix_fn_name(r, ast, node_idx) else {
        let span = ast.node(node_idx).span;
        r.diag_ctx
            .error("missing function identifier in infix call".into())
            .with_primary_span(span)
            .emit(r.diag_ctx);
        return;
    };
    match r.lookup_with_visibility(fn_name) {
        Ok(Some(sym_id)) => {
            r.node_symbols.insert(node_idx, sym_id);
        }
        Err(_) => {
            r.diag_ctx
                .error(format!(
                    "`{}` is not visible from this module",
                    str_interner::get(fn_name)
                ))
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
        }
        Ok(None) => {
            let name = str_interner::get(fn_name);
            let span = ast.node(node_idx).span;
            r.diag_ctx
                .error(format!("undefined name `{name}`"))
                .with_primary_span(span)
                .with_label(
                    span,
                    format!("`{name}` is not defined in this scope"),
                    Level::Error,
                )
                .emit(r.diag_ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// Handles statement (effect handler)
// ---------------------------------------------------------------------------

fn resolve_handles(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] effect  [1] ret_type  [2] body   multi: params
    resolve_names(r, ast, children[0]); // effect expr in enclosing scope
    r.handler_arms.insert(node_idx);
    r.push_scope(node_idx, None);
    for &param in ast.multi_children(node_idx) {
        resolve_param(r, ast, param);
    }
    resolve_names(r, ast, children[1]); // ret_type
    resolve_names(r, ast, children[2]); // body
    r.pop_scope();
}

fn resolve_let_handler_names(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] effect_node  [1] pat  [2] type_ann  [3] value
    resolve_names(r, ast, children[0]);
    if children.len() > 2 {
        resolve_names(r, ast, children[2]);
    }
    if children.len() > 3 {
        resolve_names(r, ast, children[3]);
    }
    if children.len() > 1 {
        resolve_pattern(r, ast, children[1], NodeKind::LetDecl);
        if let Some(&symbol) = r.node_symbols.get(&children[1]) {
            r.node_symbols.insert(node_idx, symbol);
        }
    }
}

// ---------------------------------------------------------------------------
// Qualified access: Projection and PropertyPattern
// ---------------------------------------------------------------------------

/// Resolve `A.b` expression (e.g. `Color.green`).
///
/// Projection children: [0] object  [1] member
fn resolve_projection(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    let object = children[0];
    let member = children[1];

    // Resolve the object (e.g. `Color`).
    resolve_names(r, ast, object);
    if !member.is_null() && ast.node(member).kind == NodeKind::Int {
        return;
    }

    // Try to resolve the member in the object's type scope.
    if !object.is_null()
        && !member.is_null()
        && let Some(&obj_sym) = r.node_symbols.get(&object)
    {
        let kind = r.symbols[obj_sym.0 as usize].kind;
        if matches!(
            kind,
            SymbolKind::Module | SymbolKind::Type | SymbolKind::Trait
        ) {
            crate::enums::record_reference(r, ast, node_idx);
            if r.enum_variants.contains_key(&node_idx)
                || crate::type_factories::deferred_variant(r, ast, object, member)
            {
                return;
            }
            let member_name = ast.node(member).str_id;
            match crate::imports::member(r, obj_sym, member_name) {
                Ok(member_sym) => {
                    r.node_symbols.insert(member, member_sym);
                    r.node_symbols.insert(node_idx, member_sym);
                }
                Err(error) => {
                    r.diag_ctx
                        .error(error)
                        .with_primary_span(ast.node(member).span)
                        .emit(r.diag_ctx);
                }
            }
        }
    }
    // Instance members are checked after the receiver's type is inferred.
}

/// Resolve `A.b` pattern (e.g. `Color.red` in pattern position).
///
/// PropertyPattern children: [0] type_name  [1] variant_name
fn resolve_property_pattern(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    resolve_names(r, ast, children[0]);
    crate::enums::record_reference(r, ast, node_idx);
    if r.enum_variants.contains_key(&node_idx)
        || crate::type_factories::deferred_variant(r, ast, children[0], children[1])
    {
        return;
    }
    if let Some(&owner) = r.node_symbols.get(&children[0]) {
        match crate::imports::member(r, owner, ast.node(children[1]).str_id) {
            Ok(symbol) => {
                r.node_symbols.insert(children[1], symbol);
                r.node_symbols.insert(node_idx, symbol);
            }
            Err(error) => {
                r.diag_ctx
                    .error(error)
                    .with_primary_span(ast.node(node_idx).span)
                    .emit(r.diag_ctx);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Default child traversal
// ---------------------------------------------------------------------------

fn resolve_children(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    for &child in ast.fixed_children(node_idx) {
        resolve_names(r, ast, child);
    }
    for &child in ast.multi_children(node_idx) {
        resolve_names(r, ast, child);
    }
}

// ---------------------------------------------------------------------------
// Pattern binding
// ---------------------------------------------------------------------------

/// Resolve a pattern and bind any names it introduces.
pub(crate) fn resolve_pattern(
    r: &mut Resolver,
    ast: &Ast,
    pattern: NodeIndex,
    declaration: NodeKind,
) {
    let refutable = matches!(declaration, NodeKind::CaseArm | NodeKind::ForLoop);
    resolve_pattern_with_mode(r, ast, pattern, declaration, refutable);
}

pub(crate) fn resolve_pattern_with_mode(
    r: &mut Resolver,
    ast: &Ast,
    pattern: NodeIndex,
    declaration: NodeKind,
    refutable: bool,
) {
    if refutable {
        let sym_kind = if declaration == NodeKind::ConstDecl {
            SymbolKind::Constant
        } else {
            SymbolKind::Variable
        };
        crate::pattern_bindings::prepare_with_kind(r, ast, pattern, sym_kind);
    }
    resolve_pattern_names(r, ast, pattern, declaration, refutable);
}

fn collect_module_binding_leaves(ast: &Ast, pat: NodeIndex, out: &mut Vec<NodeIndex>) -> bool {
    if pat.is_null() {
        return false;
    }
    match ast.node(pat).kind {
        NodeKind::Id => {
            out.push(pat);
            true
        }
        NodeKind::Underscore => true,
        NodeKind::PatternTuple | NodeKind::PatternCall => ast
            .multi_children(pat)
            .iter()
            .all(|&child| collect_module_binding_leaves(ast, child, out)),
        NodeKind::PatternTyped => {
            collect_module_binding_leaves(ast, ast.fixed_children(pat)[0], out)
        }
        NodeKind::PatternExtendedCall => ast.multi_children(pat).iter().all(|&entry| {
            if entry.is_null() {
                return false;
            }
            match ast.node(entry).kind {
                NodeKind::Id => {
                    out.push(entry);
                    true
                }
                NodeKind::Property | NodeKind::PropertyPattern => {
                    collect_module_binding_leaves(ast, ast.fixed_children(entry)[1], out)
                }
                NodeKind::PatternRestBind => {
                    let inner = ast.fixed_children(entry)[0];
                    inner.is_null() || ast.node(inner).kind == NodeKind::Underscore
                }
                _ => false,
            }
        }),
        _ => false,
    }
}

fn resolve_pattern_names(
    r: &mut Resolver,
    ast: &Ast,
    pat_idx: NodeIndex,
    _decl_kind: NodeKind,
    refutable: bool,
) {
    if pat_idx.is_null() {
        return;
    }
    r.node_scopes.insert(pat_idx, r.current_scope);
    let kind = ast.node(pat_idx).kind;
    let str_id = ast.node(pat_idx).str_id;
    let sym_kind = if _decl_kind == NodeKind::ConstDecl {
        SymbolKind::Constant
    } else {
        SymbolKind::Variable
    };

    match kind {
        NodeKind::Id => {
            if refutable
                && let Some(symbol) = r.lookup(str_id)
                && r.symbols[symbol.0 as usize].kind == SymbolKind::EnumVariant
            {
                r.node_symbols.insert(pat_idx, symbol);
                return;
            }
            bind_pattern_identifier(r, ast, pat_idx, _decl_kind);
        }
        NodeKind::PatternTyped => {
            if !refutable
                && !matches!(
                    _decl_kind,
                    NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl
                )
            {
                r.diag_ctx
                    .error("typed patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[1]);
            if !children[0].is_null() {
                if ast.node(children[0]).kind == NodeKind::Id {
                    r.node_scopes.insert(children[0], r.current_scope);
                    bind_pattern_identifier(r, ast, children[0], _decl_kind);
                } else {
                    resolve_pattern_names(r, ast, children[0], _decl_kind, refutable);
                }
            }
        }

        // Literal patterns — nothing to bind.
        NodeKind::Underscore
        | NodeKind::Int
        | NodeKind::Real
        | NodeKind::Str
        | NodeKind::Char
        | NodeKind::Bool
        | NodeKind::Null
        | NodeKind::Unit => {}

        NodeKind::PatternOr => {
            check_composite_pattern_context(r, ast, pat_idx, refutable);
            let children = ast.fixed_children(pat_idx);
            let original = r.scopes[r.current_scope.0 as usize].bindings.clone();
            resolve_pattern_names(r, ast, children[0], _decl_kind, refutable);
            // Names introduced by the left path are not available before the
            // corresponding right-path binding has actually been encountered.
            r.scopes[r.current_scope.0 as usize].bindings = original;
            resolve_pattern_names(r, ast, children[1], _decl_kind, refutable);
        }
        NodeKind::PatternAsBind => {
            check_composite_pattern_context(r, ast, pat_idx, refutable);
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind, refutable);
            let alias_node = children[1];
            r.node_scopes.insert(alias_node, r.current_scope);
            let alias_str = ast.node(alias_node).str_id;
            let sym_id = r.node_symbols.get(&alias_node).copied().unwrap_or_else(|| {
                r.define_symbol(alias_str, sym_kind, alias_node, Visibility::Private)
            });
            r.scopes[r.current_scope.0 as usize]
                .bindings
                .insert(alias_str, sym_id);
            r.node_symbols.insert(alias_node, sym_id);
        }
        NodeKind::PatternIfGuard => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind, refutable);
            resolve_names(r, ast, children[1]); // guard expression
        }
        NodeKind::PatternOptionSome => {
            if !refutable {
                r.diag_ctx
                    .error("some patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            resolve_pattern_names(
                r,
                ast,
                ast.fixed_children(pat_idx)[0],
                _decl_kind,
                refutable,
            );
        }
        NodeKind::PatternRecord => {
            r.diag_ctx
                .error(
                    "anonymous record patterns `{ ... }` require a struct type name `TypeName { ... }`"
                        .into(),
                )
                .with_primary_span(ast.node(pat_idx).span)
                .emit(r.diag_ctx);
            for &child in ast.multi_children(pat_idx) {
                if matches!(
                    ast.node(child).kind,
                    NodeKind::Property | NodeKind::PropertyPattern
                ) {
                    resolve_pattern_names(
                        r,
                        ast,
                        ast.fixed_children(child)[1],
                        _decl_kind,
                        refutable,
                    );
                } else {
                    resolve_pattern_names(r, ast, child, _decl_kind, refutable);
                }
            }
        }
        NodeKind::PatternList | NodeKind::PatternTuple => {
            if kind == NodeKind::PatternList && !refutable {
                r.diag_ctx
                    .error("List patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            if ast.node(pat_idx).kind == NodeKind::PatternTuple
                && !refutable
                && !matches!(
                    _decl_kind,
                    NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl
                )
            {
                r.diag_ctx
                    .error(
                        "tuple destructuring is supported only in declarations and parameters"
                            .into(),
                    )
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            for &child in ast.multi_children(pat_idx) {
                resolve_pattern_names(r, ast, child, _decl_kind, refutable);
            }
        }
        NodeKind::PatternCall => {
            if !refutable
                && !matches!(
                    _decl_kind,
                    NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl
                )
            {
                r.diag_ctx
                    .error(
                        "constructor patterns are supported only in match, matches, and newtype bindings"
                            .into(),
                    )
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[0]); // constructor
            for &arg in ast.multi_children(pat_idx) {
                resolve_pattern_names(r, ast, arg, _decl_kind, refutable);
            }
        }
        NodeKind::PatternExtendedCall => {
            if !refutable
                && !matches!(
                    _decl_kind,
                    NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl
                )
            {
                r.diag_ctx
                    .error(
                        "struct patterns are supported only in declarations, parameters, and match"
                            .into(),
                    )
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[0]); // struct type
            for &entry in ast.multi_children(pat_idx) {
                if entry.is_null() {
                    continue;
                }
                match ast.node(entry).kind {
                    NodeKind::Id => {
                        r.node_scopes.insert(entry, r.current_scope);
                        bind_pattern_identifier(r, ast, entry, _decl_kind);
                    }
                    NodeKind::Property | NodeKind::PropertyPattern => {
                        r.node_scopes.insert(entry, r.current_scope);
                        resolve_pattern_names(
                            r,
                            ast,
                            ast.fixed_children(entry)[1],
                            _decl_kind,
                            refutable,
                        );
                    }
                    NodeKind::PatternRestBind => {
                        r.node_scopes.insert(entry, r.current_scope);
                        let rest = ast.fixed_children(entry)[0];
                        if !rest.is_null() && ast.node(rest).kind != NodeKind::Underscore {
                            r.diag_ctx
                                .error(
                                    "struct rest pattern `...` cannot bind a variable; use `...` to ignore remaining fields"
                                        .into(),
                                )
                                .with_primary_span(ast.node(entry).span)
                                .emit(r.diag_ctx);
                        }
                    }
                    _ => resolve_pattern_names(r, ast, entry, _decl_kind, refutable),
                }
            }
        }
        NodeKind::PatternRestBind => {
            let children = ast.fixed_children(pat_idx);
            if !children[0].is_null() {
                let name_node = children[0];
                r.node_scopes.insert(name_node, r.current_scope);
                let name = ast.node(name_node).str_id;
                let symbol = r.node_symbols.get(&name_node).copied().unwrap_or_else(|| {
                    r.define_symbol(name, sym_kind, name_node, Visibility::Private)
                });
                r.scopes[r.current_scope.0 as usize]
                    .bindings
                    .insert(name, symbol);
                r.node_symbols.insert(name_node, symbol);
            }
        }
        NodeKind::PatternNot => {
            if !refutable {
                r.diag_ctx
                    .error("not patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            // A successful negation may never have reached the child's
            // bindings. Keep them private, including aliases and captures
            // referenced by internal guards.
            let previous = r.current_scope;
            r.push_scope(pat_idx, None);
            let inner = ast.fixed_children(pat_idx)[0];
            crate::pattern_bindings::prepare_with_kind(r, ast, inner, sym_kind);
            resolve_pattern_names(r, ast, inner, _decl_kind, refutable);
            r.current_scope = previous;
        }
        NodeKind::PatternErrorOk | NodeKind::PatternError | NodeKind::PatternAsync => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind, refutable);
        }
        NodeKind::PatternTypeFamily => {
            resolve_names(r, ast, ast.fixed_children(pat_idx)[0]);
        }
        NodeKind::PropertyPattern => {
            if refutable {
                resolve_property_pattern(r, ast, pat_idx);
                return;
            }
            let children = ast.fixed_children(pat_idx);
            // [0] property name (Id), [1] pattern
            resolve_pattern_names(r, ast, children[1], _decl_kind, refutable);
        }
        NodeKind::PatternFromExpr => {
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[0]);
        }
        NodeKind::PatternAndIs => {
            if !refutable {
                r.diag_ctx
                    .error("and is patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind, refutable);
            resolve_names(r, ast, children[1]);
            resolve_pattern_names(r, ast, children[2], _decl_kind, refutable);
        }
        _ => {
            // Treat unknown patterns as expressions.
            resolve_names(r, ast, pat_idx);
        }
    }
}

/// Binds a pattern identifier that is not an enum variant reference. Refutable
/// patterns reuse the symbol prepared for alternative paths in this scope.
fn bind_pattern_identifier(r: &mut Resolver, ast: &Ast, node: NodeIndex, declaration: NodeKind) {
    let name = ast.node(node).str_id;
    if matches!(
        declaration,
        NodeKind::ConstDecl
            | NodeKind::LetDecl
            | NodeKind::VarDecl
            | NodeKind::CaseArm
            | NodeKind::ForLoop
    ) && let Some(&symbol) = r.node_symbols.get(&node)
        && r.symbols[symbol.0 as usize].scope == r.current_scope
    {
        r.scopes[r.current_scope.0 as usize]
            .bindings
            .insert(name, symbol);
        return;
    }
    let kind = if declaration == NodeKind::ConstDecl {
        SymbolKind::Constant
    } else {
        SymbolKind::Variable
    };
    let symbol = r.define_symbol(name, kind, node, Visibility::Private);
    r.node_symbols.insert(node, symbol);
}

fn check_composite_pattern_context(r: &Resolver, ast: &Ast, node: NodeIndex, refutable: bool) {
    if !refutable {
        r.diag_ctx
            .error("or and as patterns are supported in match, matches and for".into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
}

// ---------------------------------------------------------------------------
// Parameter binding
// ---------------------------------------------------------------------------

/// Resolve a function/lambda parameter, binding it in the current scope.
pub(crate) fn resolve_param(r: &mut Resolver, ast: &Ast, param_idx: NodeIndex) {
    r.node_scopes.insert(param_idx, r.current_scope);
    if param_idx.is_null() {
        return;
    }
    let kind = ast.node(param_idx).kind;

    match kind {
        NodeKind::ParamSelf => {
            let str_id = str_interner::intern("self");
            let sym_id = r.define_symbol(
                str_id,
                SymbolKind::Parameter,
                param_idx,
                Visibility::Private,
            );
            r.node_symbols.insert(param_idx, sym_id);
        }
        NodeKind::ParamTyped | NodeKind::ParamCatch => {
            let children = ast.fixed_children(param_idx);
            // [0] pattern  [1] type
            resolve_names(r, ast, children[1]); // type
            resolve_pattern(r, ast, children[0], NodeKind::LetDecl);
        }
        NodeKind::ParamLambda => {
            let children = ast.fixed_children(param_idx);
            // [0] pattern  [1] type
            resolve_names(r, ast, children[1]); // type
            resolve_pattern(r, ast, children[0], NodeKind::LetDecl);
        }
        NodeKind::ParamVarargs => {
            let children = ast.fixed_children(param_idx);
            // [0] name  [1] type
            let name_node = children[0];
            let name_str = ast.node(name_node).str_id;
            let sym_id = r.define_symbol(
                name_str,
                SymbolKind::Parameter,
                name_node,
                Visibility::Private,
            );
            r.node_symbols.insert(name_node, sym_id);
            if children.len() > 1 {
                resolve_names(r, ast, children[1]);
            }
        }
        NodeKind::ParamOptional => {
            let children = ast.fixed_children(param_idx);
            // [0] name  [1] type  [2] default
            let name_node = children[0];
            let name_str = ast.node(name_node).str_id;
            let sym_id = r.define_symbol(
                name_str,
                SymbolKind::Parameter,
                name_node,
                Visibility::Private,
            );
            r.node_symbols.insert(name_node, sym_id);
            if children.len() > 1 {
                resolve_names(r, ast, children[1]);
            }
            // Defaults are resolved after all parameter names are bound, so
            // self/later references cannot accidentally resolve to outer names.
        }
        _ => {
            resolve_names(r, ast, param_idx);
        }
    }
}

// ---------------------------------------------------------------------------
// Derive definition
// ---------------------------------------------------------------------------

fn resolve_derive(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // DeriveDef: [0] type_expr  multi = trait_exprs

    // Resolve the type expression.
    resolve_names(r, ast, children[0]);
    if type_index_of_expr(r, children[0])
        .is_some_and(|ty| r.type_pool.is_reserved_collection_role(ty))
    {
        // Generic derivation inspects struct storage fields, not List elements.
        r.diag_ctx
            .error("collection traits cannot be derived from their storage layout".into())
            .with_primary_span(ast.node(node_idx).span)
            .emit(r.diag_ctx);
    } else {
        check_builtin_impl_allowed(r, ast, node_idx, children[0]);
    }

    // Resolve trait expressions.
    for &trait_expr in ast.multi_children(node_idx) {
        resolve_names(r, ast, trait_expr);
    }
    crate::json_derivation::attach_derive_associated(r, ast, r.current_scope, node_idx);
}
