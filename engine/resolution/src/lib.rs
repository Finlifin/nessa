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

mod access;
mod applications;
mod arguments;
mod associated;
mod associated_types;
mod collections;
mod comparison_derivation;
mod display_derivation;
pub use display_derivation::{DerivedDisplayPlan, DisplayDerivationMode};
mod default_body;
mod default_methods;
mod defaults;
pub use default_methods::{DefaultBodyFacts, DefaultMethodPlan};
mod effect;
mod effect_contracts;
mod enums;
mod error_coverage;
mod error_matrix;
mod error_patterns;
mod error_plans;
mod errors;
pub use comparison_derivation::DerivedComparisonPlan;
pub use error_plans::{
    ErrorConstructionPlan, ErrorConversionKind, ErrorConversionPlan, ErrorEliminationPlan,
    ErrorPatternBranch, ErrorPatternPlan, ErrorPropagationPlan, ErrorTagSource,
};
mod identity;
mod imports;
mod inference;
mod initialization;
mod source_identity;
pub use identity::SourcePackageIdentity;
pub use source_identity::scratch_package_identity;
mod list_patterns;
mod lists;
mod methods;
mod name;
mod optional;
mod ordering;
mod pattern_bindings;
mod post_do;
pub(crate) mod resolver;
mod self_provenance;
mod structs;
mod trait_loops;
pub use trait_loops::{ForLoopPlan, IteratorCall};
mod trait_schemas;
mod trait_signatures;
mod trait_typing;
mod traits;
mod tuples;
mod type_factories;
pub use enums::{EnumConstructionPlan, EnumVariantRef};
pub use type_factories::TypeFactory;
mod extended;
mod typing;
mod variadics;

use ast::{Ast, NodeIndex};
use diagnostic::{Diagnostic, DiagnosticContext};
use runtime::BuiltinFnId;
use str_interner::StrId;
use type_pool::{TypeIndex, TypePool};

use std::collections::{HashMap, HashSet};

pub use arguments::{
    CallArgumentPlan, CallArgumentValue, CallParameterBinding, argument_value_node,
};

pub use structs::{StructConstructionPlan, StructFieldBinding, StructFieldValue};

// ---------------------------------------------------------------------------
// ResolveOptions
// ---------------------------------------------------------------------------

/// Options controlling builtin access and transitional root injection.
#[derive(Debug, Clone)]
pub struct ResolveOptions {
    /// Allow `.name'builtin` views (std / core / alloc packages).
    pub builtin_access: bool,
    /// Expose builtin functions as root-scope symbols (script / test mode
    /// until prelude is fully wired).
    pub expose_root_builtins: bool,
    /// Bootstrap names for standalone resolution; normal compilation imports std.
    pub expose_root_types: bool,
    /// Exact nodes loaded from trusted package sources; names do not grant privilege.
    pub privileged_nodes: HashSet<NodeIndex>,
    /// Explicit package-root module nodes in a combined AST.
    pub package_roots: HashSet<NodeIndex>,
    /// Synthetic weak prelude imports which local declarations may shadow.
    pub implicit_imports: HashSet<NodeIndex>,
    /// Real package contexts keyed by original package-root AST nodes.
    pub package_identities: HashMap<NodeIndex, SourcePackageIdentity>,
    /// Synthetic external source containers, absent from their parent's bindings.
    /// Their children retain paths relative to the original source package.
    pub detached_package_roots: HashSet<NodeIndex>,
    /// Direct dependency candidates of each declaring package. Short-name
    /// ambiguity is diagnosed on use; no transitive or global registry fallback.
    pub package_dependencies: HashMap<NodeIndex, HashMap<StrId, Vec<NodeIndex>>>,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            builtin_access: false,
            // Transitional: keep `print(...)` working in single-file scripts.
            expose_root_builtins: true,
            expose_root_types: true,
            privileged_nodes: HashSet::new(),
            package_roots: HashSet::new(),
            implicit_imports: HashSet::new(),
            package_identities: HashMap::new(),
            detached_package_roots: HashSet::new(),
            package_dependencies: HashMap::new(),
        }
    }
}

impl ResolveOptions {
    /// Options for compiling privileged packages (`std`, `core`, `alloc`).
    pub fn for_builtin_package() -> Self {
        Self {
            builtin_access: true,
            expose_root_builtins: false,
            ..Self::default()
        }
    }
}

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
    /// A native builtin function (`CallBuiltin`), typically bound via `'builtin`.
    BuiltinFunction(BuiltinFnId),
    /// A compile-time constructor; it has no runtime TypeIndex or callable ABI.
    TypeFactory(TypeFactory),
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
    /// Handler parameter position reserved for the runtime-provided continuation.
    /// It is excluded from `param_types`, which describe effect caller arguments.
    pub continuation_param: Option<usize>,
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

/// A checked boundary conversion attached to an expression's produced value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoercionKind {
    /// Check gradual compatibility without permitting numeric narrowing.
    Assert,
    /// Perform a statically justified numeric widening or representation change.
    Convert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coercion {
    pub target: TypeIndex,
    pub kind: CoercionKind,
}

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
    pub node_coercions: HashMap<NodeIndex, Coercion>,
    pub error_constructions: HashMap<NodeIndex, crate::ErrorConstructionPlan>,
    pub error_conversions: HashMap<NodeIndex, Vec<crate::ErrorConversionPlan>>,
    pub error_propagations: HashMap<NodeIndex, crate::ErrorPropagationPlan>,
    pub error_eliminations: HashMap<NodeIndex, crate::ErrorEliminationPlan>,
    pub error_patterns: HashMap<NodeIndex, crate::ErrorPatternPlan>,
    /// Declaration-aware bindings in parameter order; explicit arguments keep
    /// their original source indices for ordered evaluation by lowering.
    pub call_arguments: HashMap<NodeIndex, CallArgumentPlan>,
    /// Static constructor calls retain their type declaration for initialization dependencies.
    pub constructor_types: HashMap<NodeIndex, SymbolId>,
    pub struct_constructions: HashMap<NodeIndex, StructConstructionPlan>,
    pub enum_constructions: HashMap<NodeIndex, EnumConstructionPlan>,
    pub enum_variants: HashMap<NodeIndex, EnumVariantRef>,
    pub for_loops: HashMap<NodeIndex, ForLoopPlan>,
    pub derived_comparisons: Vec<DerivedComparisonPlan>,
    pub display_derivations: Vec<DerivedDisplayPlan>,
    pub default_methods: Vec<DefaultMethodPlan>,
    /// Checked operator and instance method calls select a source function directly.
    pub concat_calls: HashMap<NodeIndex, SymbolId>,
    pub instance_methods: HashMap<NodeIndex, SymbolId>,
    pub application_calls: HashMap<NodeIndex, SymbolId>,
    pub update_calls: HashMap<NodeIndex, SymbolId>,
    pub node_scopes: HashMap<NodeIndex, ScopeId>,
    /// Types denoted by first-class type expressions (distinct from their value type Type).
    pub node_type_values: HashMap<NodeIndex, TypeIndex>,
    /// Resolved effect declarations.
    pub effects: Vec<EffectInfo>,
    /// Resolved trait declarations.
    pub traits: Vec<TraitInfo>,
    /// Diagnostics emitted during resolution.
    pub diagnostics: Vec<Diagnostic>,
    /// Maps SymbolId → BuiltinFnId for builtin function symbols.
    pub builtin_fns: HashMap<SymbolId, BuiltinFnId>,
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
    resolve_with_options(ast, diag_ctx, ResolveOptions::default())
}

/// Resolve with explicit options (builtin access, root injection, …).
pub fn resolve_with_options<'a>(
    ast: Ast,
    diag_ctx: &'a DiagnosticContext<'a>,
    options: ResolveOptions,
) -> ResolvedAst {
    resolver::Resolver::new(diag_ctx, options).resolve(ast)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ast::NodeKind;
    use type_pool::Intrinsic;

    fn make_ast_with_let() -> Ast {
        // Build: let x = 42
        let mut ast = Ast::new();
        ast.source = Some("let x = 42".into());
        let x_str = str_interner::intern("x");

        let int_node = {
            let mut b = ast.builder(NodeKind::Int, rustc_span::DUMMY_SP);
            b.set_str_id(str_interner::intern("42"));
            b.build()
        };
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
            b.add_child(int_node); // value
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

    #[test]
    fn builtin_view_requires_privilege() {
        let src = "const p = .print'builtin";
        let (tokens, _) = lexer::tokenize(src);
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let sf = sm.new_source_file(rustc_span::FileName::Custom("t".into()), src.to_string());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let parser = parser::Parser::new(&tokens, src, &diag, sf.start_pos);
        let ast = parser.parse();
        let resolved = resolve_with_options(ast, &diag, ResolveOptions::default());
        let _ = resolved;
        assert!(
            diag.has_errors(),
            "expected privilege error on diag_ctx for unprivileged 'builtin"
        );
    }

    #[test]
    fn builtin_view_resolves_with_access() {
        let src = "const p: fn(Any) -> Unit = .print'builtin";
        let (tokens, _) = lexer::tokenize(src);
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let sf = sm.new_source_file(rustc_span::FileName::Custom("t".into()), src.to_string());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let parser = parser::Parser::new(&tokens, src, &diag, sf.start_pos);
        let ast = parser.parse();
        let resolved = resolve_with_options(ast, &diag, ResolveOptions::for_builtin_package());
        assert!(!diag.has_errors(), "{:?}", diag.diagnostics());
        assert!(
            resolved
                .diagnostics
                .iter()
                .all(|d| d.level != diagnostic::Level::Error),
            "unexpected errors: {:?}",
            resolved.diagnostics
        );
        assert!(
            resolved.symbols.iter().any(
                |s| matches!(s.kind, SymbolKind::BuiltinFunction(id) if id == runtime::ids::PRINT)
            ),
            "expected BuiltinFunction(print) symbol"
        );
    }

    #[test]
    fn typealias_builtin_type() {
        let src = "typealias MyI64 = .i64'builtin";
        let (tokens, _) = lexer::tokenize(src);
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let sf = sm.new_source_file(rustc_span::FileName::Custom("t".into()), src.to_string());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let parser = parser::Parser::new(&tokens, src, &diag, sf.start_pos);
        let ast = parser.parse();
        let resolved = resolve_with_options(ast, &diag, ResolveOptions::for_builtin_package());
        let my = resolved
            .symbols
            .iter()
            .find(|s| str_interner::get(s.name) == "MyI64")
            .expect("MyI64 symbol");
        assert_ne!(my.type_index, type_pool::TypeIndex::INVALID);
        assert!(matches!(
            resolved.type_pool.get(my.type_index).kind,
            type_pool::TypeKind::Typealias { .. }
        ));
    }
}
