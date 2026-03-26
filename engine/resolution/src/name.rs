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
    let kind = ast.node(node_idx).kind;
    let str_id = ast.node(node_idx).str_id;

    match kind {
        // ── Leaf identifiers ───────────────────────────────────────
        NodeKind::Id => resolve_id(r, ast, node_idx, str_id),

        // ── Scope blocks ───────────────────────────────────────────
        NodeKind::Block => {
            r.push_scope(node_idx, None);
            for &child in ast.multi_children(node_idx) {
                resolve_names(r, ast, child);
            }
            r.pop_scope();
        }

        NodeKind::FileScope => {
            r.push_scope(node_idx, None);
            // Pass 1: forward-declare all top-level definitions so that
            // declaration order does not matter within a file.
            forward_declare_scope(r, ast, node_idx);
            // Pass 2: fully resolve names and bodies.
            for &child in ast.multi_children(node_idx) {
                resolve_names(r, ast, child);
            }
            r.pop_scope();
        }

        // ── Variable declarations ──────────────────────────────────
        NodeKind::LetDecl | NodeKind::ConstDecl => resolve_let_or_const(r, ast, node_idx, kind),
        NodeKind::VarDecl => resolve_var_decl(r, ast, node_idx),

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
        NodeKind::ModuleDef => resolve_module_def(r, ast, node_idx),

        // ── Impl / extend blocks ───────────────────────────────────
        NodeKind::ImplDef | NodeKind::ImplTraitDef => resolve_impl(r, ast, node_idx),
        NodeKind::ExtendDef | NodeKind::ExtendTraitDef => resolve_extend(r, ast, node_idx),

        // ── Derive ─────────────────────────────────────────────────
        NodeKind::DeriveDef => resolve_derive(r, ast, node_idx),

        // ── Visibility wrapper ─────────────────────────────────────
        NodeKind::PubDef => resolve_pub_def(r, ast, node_idx),

        // ── Use statement ──────────────────────────────────────────
        NodeKind::UseStatement => {
            // TODO(3a): resolve use paths, import symbols into current scope
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
        NodeKind::ForLoop => resolve_for_loop(r, ast, node_idx),
        NodeKind::WhileLoop => resolve_while_loop(r, ast, node_idx),
        NodeKind::IfStatement => resolve_if(r, ast, node_idx),

        // ── Effect handler ─────────────────────────────────────────
        NodeKind::HandlesStatement => resolve_handles(r, ast, node_idx),

        // ── Qualified access ───────────────────────────────────────
        NodeKind::Projection => resolve_projection(r, ast, node_idx),
        NodeKind::PropertyPattern => resolve_property_pattern(r, ast, node_idx),

        // ── Default: recurse ───────────────────────────────────────
        _ => resolve_children(r, ast, node_idx),
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

fn forward_declare_one(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    if node_idx.is_null() {
        return;
    }
    let kind = ast.node(node_idx).kind;
    match kind {
        NodeKind::FunctionDef => {
            let children = ast.fixed_children(node_idx);
            let name_str = ast.node(children[0]).str_id;
            r.define_symbol(
                name_str,
                SymbolKind::Function,
                node_idx,
                Visibility::Package,
            );
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
            let sym_id =
                r.define_symbol(name_str, SymbolKind::Module, node_idx, Visibility::Package);
            r.symbol_mut(sym_id).type_index = type_idx;
        }
        NodeKind::PubDef => {
            // Unwrap visibility wrapper and forward-declare the inner definition.
            let children = ast.fixed_children(node_idx);
            if !children.is_empty() {
                forward_declare_one(r, ast, children[0]);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Identifier resolution
// ---------------------------------------------------------------------------

fn resolve_id(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex, str_id: str_interner::StrId) {
    if let Some(sym_id) = r.lookup(str_id) {
        r.node_symbols.insert(node_idx, sym_id);
    } else {
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

    // If not already forward-declared, define now (e.g. nested functions).
    let sym_id = if let Some(existing) = r.lookup_current_scope(name_str) {
        existing
    } else {
        r.define_symbol(
            name_str,
            SymbolKind::Function,
            node_idx,
            Visibility::Package,
        )
    };
    r.node_symbols.insert(name_node, sym_id);

    // Function body gets its own scope.
    r.push_scope(node_idx, None);

    // Bind parameters.
    for &param in ast.multi_children(node_idx) {
        resolve_param(r, ast, param);
    }
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
    r.push_scope(node_idx, Some(type_idx));
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.pop_scope();
}

fn resolve_struct_field(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] type  [2] default
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;
    let sym_id = r.define_symbol(name_str, SymbolKind::Field, node_idx, Visibility::Package);
    r.node_symbols.insert(name_node, sym_id);

    resolve_names(r, ast, children[1]); // type
    resolve_names(r, ast, children[2]); // default value
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

    r.push_scope(node_idx, Some(type_idx));
    let mut variant_index = 0u32;
    for &member in ast.multi_children(node_idx) {
        if ast.node(member).kind == NodeKind::EnumVariant {
            resolve_enum_variant(r, ast, member, variant_index);
            variant_index += 1;
        } else {
            resolve_names(r, ast, member);
        }
    }
    r.pop_scope();
}

fn resolve_enum_variant(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex, variant_index: u32) {
    let children = ast.fixed_children(node_idx);
    // [0] name   multi: params (variant fields)
    let name_node = children[0];
    let name_str = ast.node(name_node).str_id;
    let sym_id = r.define_symbol(
        name_str,
        SymbolKind::EnumVariant,
        node_idx,
        Visibility::Package,
    );
    r.node_symbols.insert(name_node, sym_id);
    r.enum_variant_indices.insert(sym_id, variant_index);

    for &field in ast.multi_children(node_idx) {
        resolve_names(r, ast, field);
    }
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

    // Resolve parent trait expressions.
    if !parents_node.is_null() {
        resolve_names(r, ast, parents_node);
    }

    r.push_scope(node_idx, Some(type_idx));
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

    let (sym_id, type_idx) = if let Some(existing) = r.lookup_current_scope(name_str) {
        let ti = r.symbols[existing.0 as usize].type_index;
        (existing, ti)
    } else {
        let type_idx = r.register_type(TypeKind::Module { name: name_str });
        let sym_id = r.define_symbol(name_str, SymbolKind::Module, node_idx, Visibility::Package);
        r.symbol_mut(sym_id).type_index = type_idx;
        (sym_id, type_idx)
    };
    r.node_symbols.insert(name_node, sym_id);

    r.push_scope(node_idx, Some(type_idx));
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.pop_scope();
}

// ---------------------------------------------------------------------------
// Impl / Extend blocks
// ---------------------------------------------------------------------------

fn resolve_impl(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    // ImplDef:      [0] type_expr            multi: members
    // ImplTraitDef: [0] trait_expr [1] type   multi: members
    r.push_scope(node_idx, None);
    for &child in ast.fixed_children(node_idx) {
        resolve_names(r, ast, child);
    }
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.pop_scope();
}

fn resolve_extend(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    // Same structure as impl.
    r.push_scope(node_idx, None);
    for &child in ast.fixed_children(node_idx) {
        resolve_names(r, ast, child);
    }
    for &member in ast.multi_children(node_idx) {
        resolve_names(r, ast, member);
    }
    r.pop_scope();
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
    let sym_count_before = r.symbols.len();
    resolve_names(r, ast, inner);
    // Promote the first symbol defined by the inner definition to Public.
    if r.symbols.len() > sym_count_before {
        r.symbols[sym_count_before].visibility = Visibility::Public;
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
    resolve_names(r, ast, children[1]); // return type
    resolve_names(r, ast, children[0]); // body
    r.pop_scope();
}

// ---------------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------------

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

    // Try to resolve the member in the object's type scope.
    if !object.is_null() && !member.is_null() {
        if let Some(&obj_sym) = r.node_symbols.get(&object) {
            let sym = &r.symbols[obj_sym.0 as usize];
            if sym.type_index != type_pool::TypeIndex::INVALID {
                let member_name = ast.node(member).str_id;
                if let Some(member_sym) = r.lookup_in_type_scope(sym.type_index, member_name) {
                    r.node_symbols.insert(member, member_sym);
                    return;
                }
            }
        }
    }
    // Fallback: resolve member as a regular name.
    resolve_names(r, ast, member);
}

/// Resolve `A.b` pattern (e.g. `Color.red` in pattern position).
///
/// PropertyPattern children: [0] type_name  [1] variant_name
fn resolve_property_pattern(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    let type_node = children[0];
    let variant_node = children[1];

    // Resolve the type name (e.g. `Color`).
    resolve_names(r, ast, type_node);

    // Resolve the variant name in the type's scope.
    if !type_node.is_null() && !variant_node.is_null() {
        if let Some(&type_sym) = r.node_symbols.get(&type_node) {
            let sym = &r.symbols[type_sym.0 as usize];
            if sym.type_index != type_pool::TypeIndex::INVALID {
                let variant_name = ast.node(variant_node).str_id;
                if let Some(variant_sym) = r.lookup_in_type_scope(sym.type_index, variant_name) {
                    r.node_symbols.insert(variant_node, variant_sym);
                    return;
                }
            }
        }
    }
    // Fallback: resolve variant as a regular name.
    resolve_names(r, ast, variant_node);
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
    pat_idx: NodeIndex,
    _decl_kind: NodeKind,
) {
    if pat_idx.is_null() {
        return;
    }
    let kind = ast.node(pat_idx).kind;
    let str_id = ast.node(pat_idx).str_id;

    match kind {
        NodeKind::Id => {
            let sym_id =
                r.define_symbol(str_id, SymbolKind::Variable, pat_idx, Visibility::Private);
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
            let children = ast.fixed_children(pat_idx);
            resolve_pattern(r, ast, children[0], _decl_kind);
            resolve_pattern(r, ast, children[1], _decl_kind);
        }
        NodeKind::PatternAsBind => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern(r, ast, children[0], _decl_kind);
            let alias_node = children[1];
            let alias_str = ast.node(alias_node).str_id;
            let sym_id = r.define_symbol(
                alias_str,
                SymbolKind::Variable,
                alias_node,
                Visibility::Private,
            );
            r.node_symbols.insert(alias_node, sym_id);
        }
        NodeKind::PatternIfGuard => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern(r, ast, children[0], _decl_kind);
            resolve_names(r, ast, children[1]); // guard expression
        }
        NodeKind::PatternList | NodeKind::PatternTuple | NodeKind::PatternRecord => {
            for &child in ast.multi_children(pat_idx) {
                resolve_pattern(r, ast, child, _decl_kind);
            }
        }
        NodeKind::PatternCall | NodeKind::PatternExtendedCall => {
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[0]); // constructor
            for &arg in ast.multi_children(pat_idx) {
                resolve_pattern(r, ast, arg, _decl_kind);
            }
        }
        NodeKind::PatternRestBind => {
            let children = ast.fixed_children(pat_idx);
            if !children[0].is_null() {
                let name_node = children[0];
                let name_str = ast.node(name_node).str_id;
                let sym_id = r.define_symbol(
                    name_str,
                    SymbolKind::Variable,
                    name_node,
                    Visibility::Private,
                );
                r.node_symbols.insert(name_node, sym_id);
            }
        }
        NodeKind::PatternOptionSome
        | NodeKind::PatternErrorOk
        | NodeKind::PatternNot
        | NodeKind::PatternError
        | NodeKind::PatternAsync => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern(r, ast, children[0], _decl_kind);
        }
        NodeKind::PropertyPattern => {
            let children = ast.fixed_children(pat_idx);
            // [0] property name (Id), [1] pattern
            resolve_pattern(r, ast, children[1], _decl_kind);
        }
        NodeKind::PatternFromExpr => {
            let children = ast.fixed_children(pat_idx);
            resolve_names(r, ast, children[0]);
        }
        NodeKind::PatternAndIs => {
            let children = ast.fixed_children(pat_idx);
            resolve_pattern(r, ast, children[0], _decl_kind);
            resolve_pattern(r, ast, children[1], _decl_kind);
        }
        _ => {
            // Treat unknown patterns as expressions.
            resolve_names(r, ast, pat_idx);
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter binding
// ---------------------------------------------------------------------------

/// Resolve a function/lambda parameter, binding it in the current scope.
pub(crate) fn resolve_param(r: &mut Resolver, ast: &Ast, param_idx: NodeIndex) {
    if param_idx.is_null() {
        return;
    }
    let kind = ast.node(param_idx).kind;

    match kind {
        NodeKind::ParamSelf => {
            let str_id = ast.node(param_idx).str_id;
            let sym_id = r.define_symbol(
                str_id,
                SymbolKind::Parameter,
                param_idx,
                Visibility::Private,
            );
            r.node_symbols.insert(param_idx, sym_id);
        }
        NodeKind::ParamTyped => {
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
            if children.len() > 2 {
                resolve_names(r, ast, children[2]);
            }
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

    // Resolve trait expressions.
    for &trait_expr in ast.multi_children(node_idx) {
        resolve_names(r, ast, trait_expr);
    }
}
