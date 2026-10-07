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
                if matches!(
                    ast.node(child).kind,
                    NodeKind::ExtendDef | NodeKind::ExtendTraitDef
                ) {
                    prepare_scope_tree(r, ast, child, None);
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

        // ── Typealias ──────────────────────────────────────────────
        NodeKind::Typealias => resolve_typealias(r, ast, node_idx),
        NodeKind::Newtype => {
            r.diag_ctx
                .error("source newtype semantics are not implemented".into())
                .with_primary_span(ast.node(node_idx).span)
                .emit(r.diag_ctx);
        }

        // ── View (e.g. `.print'builtin`) ───────────────────────────
        NodeKind::View => resolve_view(r, ast, node_idx),

        // ── Symbol literal `.id` — name is not a scope lookup ──────
        NodeKind::Symbol => {}

        // ── Derive ─────────────────────────────────────────────────
        NodeKind::DeriveDef => resolve_derive(r, ast, node_idx),

        // ── Visibility wrapper ─────────────────────────────────────
        NodeKind::PubDef | NodeKind::PrivateDef => resolve_pub_def(r, ast, node_idx),

        // ── Use statement ──────────────────────────────────────────
        NodeKind::UseStatement => {
            crate::imports::resolve_local_use(r, ast, node_idx, Visibility::Package);
        }

        // ── Lambda ─────────────────────────────────────────────────
        NodeKind::Lambda => resolve_lambda(r, ast, node_idx),

        // ── Named argument ─────────────────────────────────────────
        NodeKind::NamedArg => {
            // children[0] = parameter name (Id) — do NOT resolve as variable
            // children[1] = value expression — resolve normally
            let children = ast.fixed_children(node_idx);
            resolve_names(r, ast, children[1]);
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

        // ── Effect handler ─────────────────────────────────────────
        NodeKind::HandlesStatement => resolve_handles(r, ast, node_idx),

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
        let pattern = children[0];
        let span = ast.node(pattern).span;
        if ast.node(pattern).kind != NodeKind::PatternCall {
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
        if !seen.insert(symbol) {
            r.diag_ctx
                .error("duplicate handler for the same effect".into())
                .with_primary_span(span)
                .emit(r.diag_ctx);
        }
        let expected = ast.multi_children(effect.def_node).len();
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
        for &param in params {
            if !matches!(ast.node(param).kind, NodeKind::Id | NodeKind::Underscore) {
                r.diag_ctx
                    .error("handler parameter destructuring is not yet implemented".into())
                    .with_primary_span(ast.node(param).span)
                    .emit(r.diag_ctx);
            }
            resolve_pattern(r, ast, param, NodeKind::LetDecl);
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
            let name = ast.fixed_children(node_idx)[0];
            if ast.node(name).kind == NodeKind::Id {
                r.define_symbol(
                    ast.node(name).str_id,
                    if kind == NodeKind::ConstDecl {
                        SymbolKind::Constant
                    } else {
                        SymbolKind::Variable
                    },
                    node_idx,
                    Visibility::Package,
                );
            } else {
                r.diag_ctx
                    .error("module value destructuring is not implemented".into())
                    .with_primary_span(ast.node(name).span)
                    .emit(r.diag_ctx);
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
                if let Some(&symbol) = r.node_symbols.get(&children[0]) {
                    r.symbol_mut(symbol).visibility = if kind == NodeKind::PrivateDef {
                        Visibility::Private
                    } else {
                        Visibility::Public
                    };
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
    // Resolve value first (before binding the pattern).
    if children.len() > 2 {
        resolve_names(r, ast, children[2]);
    }
    if children.len() > 1 {
        resolve_names(r, ast, children[1]);
    }
    resolve_pattern(r, ast, children[0], kind);
    if let Some(&symbol) = r.node_symbols.get(&children[0]) {
        r.node_symbols.insert(node_idx, symbol);
    }

    // If `const name = .foo'builtin`, promote the binding to BuiltinFunction
    // so calls lower to CallBuiltin.
    if kind == NodeKind::ConstDecl && children.len() > 2 {
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

    if children.len() > 3 {
        resolve_names(r, ast, children[3]);
    }
}

/// `var pat: Type = value`
fn resolve_var_decl(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] pattern  [1] type  [2] value
    if children.len() > 2 {
        resolve_names(r, ast, children[2]);
    }
    if children.len() > 1 {
        resolve_names(r, ast, children[1]);
    }
    resolve_pattern(r, ast, children[0], NodeKind::VarDecl);
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
    resolve_names(r, ast, object);

    let view_name = str_interner::get(ast.node(view_id_node).str_id);
    if view_name != "builtin" {
        // Other views (`'type`, …) are not resolved here yet.
        return;
    }

    if !r.options.builtin_access && !r.options.privileged_nodes.contains(&node_idx) {
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

// ---------------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------------

fn validate_control_placement(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) {
    let kind = ast.node(node).kind;
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
            if matches!(
                owner,
                NodeKind::FunctionDef | NodeKind::TraitDeriveFn | NodeKind::Lambda
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
        let keyword = match kind {
            NodeKind::OptionPropagation => "?",
            NodeKind::ErrorPropagation => "!",
            NodeKind::ReturnStatement => "return",
            NodeKind::ResumeStatement => "resume",
            NodeKind::BreakStatement => "break",
            _ => "continue",
        };
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

// ---------------------------------------------------------------------------
// Handles statement (effect handler)
// ---------------------------------------------------------------------------

fn resolve_handles(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] effect  [1] ret_type  [2] body   multi: params
    r.push_scope(node_idx, None);
    resolve_names(r, ast, children[0]); // effect expr
    for &param in ast.multi_children(node_idx) {
        resolve_param(r, ast, param);
    }
    resolve_names(r, ast, children[1]); // ret_type
    resolve_names(r, ast, children[2]); // body
    r.pop_scope();
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
    if matches!(declaration, NodeKind::CaseArm | NodeKind::ForLoop) {
        crate::pattern_bindings::prepare(r, ast, pattern);
    }
    resolve_pattern_names(r, ast, pattern, declaration);
}

fn resolve_pattern_names(r: &mut Resolver, ast: &Ast, pat_idx: NodeIndex, _decl_kind: NodeKind) {
    if pat_idx.is_null() {
        return;
    }
    r.node_scopes.insert(pat_idx, r.current_scope);
    let kind = ast.node(pat_idx).kind;
    let str_id = ast.node(pat_idx).str_id;

    match kind {
        NodeKind::Id => {
            if matches!(_decl_kind, NodeKind::CaseArm | NodeKind::ForLoop)
                && let Some(symbol) = r.lookup(str_id)
                && r.symbols[symbol.0 as usize].kind == SymbolKind::EnumVariant
            {
                r.node_symbols.insert(pat_idx, symbol);
                return;
            }
            if matches!(
                _decl_kind,
                NodeKind::ConstDecl
                    | NodeKind::LetDecl
                    | NodeKind::VarDecl
                    | NodeKind::CaseArm
                    | NodeKind::ForLoop
            ) && let Some(&symbol) = r.node_symbols.get(&pat_idx)
                && r.symbols[symbol.0 as usize].scope == r.current_scope
            {
                r.scopes[r.current_scope.0 as usize]
                    .bindings
                    .insert(str_id, symbol);
                return;
            }
            let kind = if _decl_kind == NodeKind::ConstDecl {
                SymbolKind::Constant
            } else {
                SymbolKind::Variable
            };
            let sym_id = r.define_symbol(str_id, kind, pat_idx, Visibility::Private);
            r.node_symbols.insert(pat_idx, sym_id);
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
            check_composite_pattern_context(r, ast, pat_idx, _decl_kind);
            let children = ast.fixed_children(pat_idx);
            let original = r.scopes[r.current_scope.0 as usize].bindings.clone();
            resolve_pattern_names(r, ast, children[0], _decl_kind);
            // Names introduced by the left path are not available before the
            // corresponding right-path binding has actually been encountered.
            r.scopes[r.current_scope.0 as usize].bindings = original;
            resolve_pattern_names(r, ast, children[1], _decl_kind);
        }
        NodeKind::PatternAsBind => {
            check_composite_pattern_context(r, ast, pat_idx, _decl_kind);
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind);
            let alias_node = children[1];
            r.node_scopes.insert(alias_node, r.current_scope);
            let alias_str = ast.node(alias_node).str_id;
            let sym_id = r.node_symbols.get(&alias_node).copied().unwrap_or_else(|| {
                r.define_symbol(
                    alias_str,
                    SymbolKind::Variable,
                    alias_node,
                    Visibility::Private,
                )
            });
            r.scopes[r.current_scope.0 as usize]
                .bindings
                .insert(alias_str, sym_id);
            r.node_symbols.insert(alias_node, sym_id);
        }
        NodeKind::PatternIfGuard => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind);
            resolve_names(r, ast, children[1]); // guard expression
        }
        NodeKind::PatternOptionSome => {
            if !matches!(_decl_kind, NodeKind::CaseArm | NodeKind::ForLoop) {
                r.diag_ctx
                    .error("some patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            resolve_pattern_names(r, ast, ast.fixed_children(pat_idx)[0], _decl_kind);
        }
        NodeKind::PatternList | NodeKind::PatternTuple | NodeKind::PatternRecord => {
            if kind == NodeKind::PatternList
                && !matches!(_decl_kind, NodeKind::CaseArm | NodeKind::ForLoop)
            {
                r.diag_ctx
                    .error("List patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            if ast.node(pat_idx).kind == NodeKind::PatternTuple
                && !matches!(
                    _decl_kind,
                    NodeKind::LetDecl
                        | NodeKind::VarDecl
                        | NodeKind::ConstDecl
                        | NodeKind::CaseArm
                        | NodeKind::ForLoop
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
                resolve_pattern_names(r, ast, child, _decl_kind);
            }
        }
        NodeKind::PatternCall | NodeKind::PatternExtendedCall => {
            if !matches!(_decl_kind, NodeKind::CaseArm | NodeKind::ForLoop) {
                r.diag_ctx
                    .error(
                        "constructor patterns are supported only in match and matches expressions"
                            .into(),
                    )
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[0]); // constructor
            for &arg in ast.multi_children(pat_idx) {
                resolve_pattern_names(r, ast, arg, _decl_kind);
            }
        }
        NodeKind::PatternRestBind => {
            let children = ast.fixed_children(pat_idx);
            if !children[0].is_null() {
                let name_node = children[0];
                r.node_scopes.insert(name_node, r.current_scope);
                let name = ast.node(name_node).str_id;
                let symbol = r.node_symbols.get(&name_node).copied().unwrap_or_else(|| {
                    r.define_symbol(name, SymbolKind::Variable, name_node, Visibility::Private)
                });
                r.scopes[r.current_scope.0 as usize]
                    .bindings
                    .insert(name, symbol);
                r.node_symbols.insert(name_node, symbol);
            }
        }
        NodeKind::PatternNot => {
            if !matches!(_decl_kind, NodeKind::CaseArm | NodeKind::ForLoop) {
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
            crate::pattern_bindings::prepare(r, ast, inner);
            resolve_pattern_names(r, ast, inner, _decl_kind);
            r.current_scope = previous;
        }
        NodeKind::PatternErrorOk | NodeKind::PatternError | NodeKind::PatternAsync => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind);
        }
        NodeKind::PatternTypeFamily => {
            resolve_names(r, ast, ast.fixed_children(pat_idx)[0]);
        }
        NodeKind::PropertyPattern => {
            if matches!(_decl_kind, NodeKind::CaseArm | NodeKind::ForLoop) {
                resolve_property_pattern(r, ast, pat_idx);
                return;
            }
            let children = ast.fixed_children(pat_idx);
            // [0] property name (Id), [1] pattern
            resolve_pattern_names(r, ast, children[1], _decl_kind);
        }
        NodeKind::PatternFromExpr => {
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[0]);
        }
        NodeKind::PatternAndIs => {
            if !matches!(_decl_kind, NodeKind::CaseArm | NodeKind::ForLoop) {
                r.diag_ctx
                    .error("and is patterns are supported in match, matches and for".into())
                    .with_primary_span(ast.node(pat_idx).span)
                    .emit(r.diag_ctx);
            }
            let children = ast.fixed_children(pat_idx);
            resolve_pattern_names(r, ast, children[0], _decl_kind);
            resolve_names(r, ast, children[1]);
            resolve_pattern_names(r, ast, children[2], _decl_kind);
        }
        _ => {
            // Treat unknown patterns as expressions.
            resolve_names(r, ast, pat_idx);
        }
    }
}

fn check_composite_pattern_context(
    r: &Resolver,
    ast: &Ast,
    node: NodeIndex,
    declaration: NodeKind,
) {
    if !matches!(declaration, NodeKind::CaseArm | NodeKind::ForLoop) {
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
}
