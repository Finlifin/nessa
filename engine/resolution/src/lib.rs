//! Resolution — semantic analysis for the nessa language.
//!
//! Transforms a raw AST into a [`ResolvedAst`] by performing four phases:
//!
//! | Phase | Module     | Description                                    |
//! |-------|------------|------------------------------------------------|
//! | 3a    | [`name`]   | Build scope tree, bind all identifiers         |
//! | 3b    | [`typing`] | Infer types for expressions                    |
//! | 3c    | [`effect`] | Collect effect declarations                    |
//! | 3d    | [`traits`] | Collect trait info, record impl relationships  |

mod effect;
mod name;
pub(crate) mod resolver;
mod traits;
mod typing;

use ast::{Ast, NodeIndex};
use diagnostic::{Diagnostic, DiagnosticContext};
use nsbc::IntrinsicFn;
use str_interner::StrId;
use type_pool::{TypeIndex, TypePool};

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Visibility
// ---------------------------------------------------------------------------

/// Three-level visibility model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// `pub` — accessible everywhere.
    Public,
    /// Default — accessible within the same package.
    Package,
    /// `private` — accessible only in the defining scope.
    Private,
}

// ---------------------------------------------------------------------------
// Symbol — a resolved binding in the symbol table
// ---------------------------------------------------------------------------

/// A unique identifier for a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

/// Category of a resolved symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Variable,
    Constant,
    Function,
    /// An intrinsic function implemented natively by the VM.
    IntrinsicFunction(IntrinsicFn),
    Type,
    Module,
    Effect,
    Trait,
    EnumVariant,
    Field,
    Parameter,
    Label,
}

/// A resolved symbol.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: StrId,
    pub kind: SymbolKind,
    /// Which scope this symbol is defined in.
    pub scope: ScopeId,
    /// Type of this symbol (may be `TypeIndex::INVALID` until type resolution).
    pub type_index: TypeIndex,
    /// Visibility of this symbol.
    pub visibility: Visibility,
    /// AST node where this symbol was defined.
    pub def_node: NodeIndex,
}

// ---------------------------------------------------------------------------
// Scope — a node in the scope tree
// ---------------------------------------------------------------------------

/// Unique identifier for a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(pub u32);

impl ScopeId {
    pub const ROOT: Self = Self(0);
}

/// A scope in the scope tree.
#[derive(Debug, Clone)]
pub struct Scope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    /// Symbols defined directly in this scope, keyed by name.
    pub bindings: HashMap<StrId, SymbolId>,
    /// Child scopes.
    pub children: Vec<ScopeId>,
    /// If this scope is an associated scope of a type, which type.
    pub assoc_type: Option<TypeIndex>,
    /// Node that introduced this scope.
    pub node: NodeIndex,
}

// ---------------------------------------------------------------------------
// EffectInfo — a resolved effect declaration
// ---------------------------------------------------------------------------

/// A resolved effect declaration.
#[derive(Debug, Clone)]
pub struct EffectInfo {
    pub name: StrId,
    pub type_index: TypeIndex,
    pub operations: Vec<EffectOperation>,
    pub is_async: bool,
}

/// A single operation within an effect.
#[derive(Debug, Clone)]
pub struct EffectOperation {
    pub name: StrId,
    pub param_types: Vec<TypeIndex>,
    pub return_type: TypeIndex,
}

// ---------------------------------------------------------------------------
// TraitInfo — resolved trait information
// ---------------------------------------------------------------------------

/// Information about a resolved trait definition.
#[derive(Debug, Clone)]
pub struct TraitInfo {
    pub name: StrId,
    pub type_index: TypeIndex,
    pub required_methods: Vec<StrId>,
    pub derived_methods: Vec<StrId>,
    pub parent_traits: Vec<TypeIndex>,
}

// ---------------------------------------------------------------------------
// ResolvedAst — output of the resolution phase
// ---------------------------------------------------------------------------

/// Mapping from AST node to resolved symbol.
pub type NodeSymbolMap = HashMap<NodeIndex, SymbolId>;

/// Mapping from AST node to resolved type.
pub type NodeTypeMap = HashMap<NodeIndex, TypeIndex>;

/// The output of the resolution phase.
pub struct ResolvedAst {
    /// The original AST (unchanged).
    pub ast: Ast,
    /// The type pool (built during resolution).
    pub type_pool: TypePool,
    /// All scopes in the program.
    pub scopes: Vec<Scope>,
    /// All resolved symbols.
    pub symbols: Vec<Symbol>,
    /// Mapping from identifier AST nodes to their resolved symbol.
    pub node_symbols: NodeSymbolMap,
    /// Mapping from expression AST nodes to their inferred type.
    pub node_types: NodeTypeMap,
    /// Resolved effect declarations.
    pub effects: Vec<EffectInfo>,
    /// Resolved trait declarations.
    pub traits: Vec<TraitInfo>,
    /// Diagnostics emitted during resolution.
    pub diagnostics: Vec<Diagnostic>,
    /// Maps SymbolId → IntrinsicFn for symbols that are intrinsic functions.
    pub intrinsic_fns: HashMap<SymbolId, IntrinsicFn>,
    /// Maps enum variant SymbolId → variant index (0-based).
    pub enum_variant_indices: HashMap<SymbolId, u32>,
    /// Maps Projection AST node → field index in the struct type.
    /// Populated during type resolution when the LHS is a known struct type.
    pub node_field_indices: HashMap<NodeIndex, u32>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Resolve an AST, producing a fully resolved AST with symbol table and type pool.
pub fn resolve<'a>(ast: Ast, diag_ctx: &'a DiagnosticContext<'a>) -> ResolvedAst {
    resolver::Resolver::new(diag_ctx).resolve(ast)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ast::{NodeKind};
    use type_pool::Intrinsic;

    fn make_ast_with_let() -> Ast {
        // Build: let x = 42
        let mut ast = Ast::new();
        let interner = &mut str_interner::Interner::new();
        let x_str = interner.intern("x");

        let int_node = ast.builder(NodeKind::Int, rustc_span::DUMMY_SP).build();
        let id_node = {
            let mut b = ast.builder(NodeKind::Id, rustc_span::DUMMY_SP);
            b.set_str_id(x_str);
            b.build()
        };
        // LetDecl: [0] pattern [1] type [2] value [3] else
        let let_node = {
            let mut b = ast.builder(NodeKind::LetDecl, rustc_span::DUMMY_SP);
            b.add_child(id_node);
            b.add_child(NodeIndex::NULL); // no type
            b.add_child(int_node);        // value
            b.add_child(NodeIndex::NULL); // no else
            b.build()
        };
        let root = {
            let mut b = ast.builder(NodeKind::FileScope, rustc_span::DUMMY_SP);
            b.add_multi_children(&[let_node]);
            b.build()
        };
        ast.root = root;
        ast
    }

    #[test]
    fn resolver_creates_scope_and_symbol() {
        let ast = make_ast_with_let();
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let resolved = resolve(ast, &diag);
        assert!(
            !resolved.symbols.is_empty(),
            "should have at least one symbol"
        );
        // Find the user-defined variable "x" (intrinsic functions are registered first).
        let x_sym = resolved
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Variable)
            .expect("should have a Variable symbol for 'x'");
        assert_eq!(x_sym.kind, SymbolKind::Variable);
        assert!(!resolved.scopes.is_empty());
    }

    #[test]
    fn resolver_infers_int_literal_type() {
        let ast = make_ast_with_let();
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let resolved = resolve(ast, &diag);
        let int_types: Vec<_> = resolved
            .node_types
            .values()
            .filter(|&&ti| ti == Intrinsic::I64.type_index())
            .collect();
        assert!(!int_types.is_empty(), "should infer i64 for int literal");
    }

    #[test]
    fn resolver_emits_no_errors_for_valid() {
        let ast = make_ast_with_let();
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let resolved = resolve(ast, &diag);
        assert!(
            resolved.diagnostics.is_empty(),
            "no errors expected, got: {:?}",
            resolved.diagnostics
        );
    }
}
