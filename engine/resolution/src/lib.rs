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
mod assignments;
mod associated;
mod associated_types;
mod capability;
pub use capability::{
    CapabilityDecl, HandlerApplicationPlan, HandlerBindingKind, HandlesStmtPlan, LetHandlerPlan,
};
mod collection_inference;
mod collections;
mod comparison_derivation;
mod display_derivation;
mod flow_narrowing;
pub use display_derivation::DerivedDisplayPlan;
mod hash_derivation;
pub use hash_derivation::DerivedHashPlan;
mod json_derivation;
pub use json_derivation::{DerivedJsonPlan, JsonNirTypes};
mod default_body;
mod default_methods;
mod defaults;
pub use default_methods::{DefaultBodyFacts, DefaultMethodPlan};
mod effect;
mod effect_contracts;
mod enums;
mod equality;
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
mod newtype;
mod optional;
mod ordering;
mod pattern_bindings;
mod post_do;
mod ranges;
pub(crate) mod resolver;
mod self_provenance;
mod structs;
mod suggestions;
mod task_combinators;
mod trait_loops;
pub use trait_loops::{ForLoopPlan, IteratorCall, StaticForLoop};
mod trait_schemas;
mod trait_signatures;
mod trait_typing;
mod traits;
mod trusted;
mod tuples;
mod type_factories;
mod type_patterns;
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
    CallArgumentPlan, CallArgumentValue, CallParameterBinding, SpreadPackPlan, argument_value_node,
};

pub use structs::{
    StructConstructionPlan, StructFieldBinding, StructFieldValue, StructPatternPlan,
};

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
    /// Exact AST nodes loaded from packages declaring `[ffi] allow = true`.
    pub ffi_nodes: HashSet<NodeIndex>,
    /// Restricted `[ffi] libraries = [...]` allowlist per AST node when non-empty.
    pub ffi_library_allowlists: HashMap<NodeIndex, HashSet<String>>,
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
            ffi_nodes: HashSet::new(),
            ffi_library_allowlists: HashMap::new(),
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
    pub struct_patterns: HashMap<NodeIndex, StructPatternPlan>,
    pub enum_constructions: HashMap<NodeIndex, EnumConstructionPlan>,
    pub enum_variants: HashMap<NodeIndex, EnumVariantRef>,
    pub newtype_constructions: HashMap<NodeIndex, TypeIndex>,
    pub newtype_wraps: HashMap<NodeIndex, TypeIndex>,
    pub newtype_patterns: HashMap<NodeIndex, TypeIndex>,
    pub for_loops: HashMap<NodeIndex, ForLoopPlan>,
    pub derived_comparisons: Vec<DerivedComparisonPlan>,
    pub display_derivations: Vec<DerivedDisplayPlan>,
    pub hash_derivations: Vec<DerivedHashPlan>,
    pub json_derivations: Vec<DerivedJsonPlan>,
    pub default_methods: Vec<DefaultMethodPlan>,
    pub capabilities: Vec<CapabilityDecl>,
    pub handler_applications: HashMap<NodeIndex, HandlerApplicationPlan>,
    pub handles_statements: HashMap<NodeIndex, HandlesStmtPlan>,
    pub let_handlers: HashMap<NodeIndex, LetHandlerPlan>,
    /// Checked operator and instance method calls select a source function directly.
    pub concat_calls: HashMap<NodeIndex, SymbolId>,
    /// `==`/`!=` between Any or tuple operands call the trusted std equality.
    pub equality_calls: HashMap<NodeIndex, SymbolId>,
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
    /// Call nodes produced by normalizing `lhs |> rhs`, where `lhs` (`args[0]`)
    /// must be evaluated before a dynamic callee or receiver expression.
    pub pipeline_calls: HashSet<NodeIndex>,
    /// `PatternTyped` nodes used as static type annotations (such as standalone
    /// `case (x: T) => ...` parameter annotations or typed declarations) rather
    /// than dynamic `Any`/trait runtime type tests.
    pub annotation_typed_patterns: HashSet<NodeIndex>,
}

/// Flatten a `CaseAlternative` / `CaseMap` tree left-to-right into `CaseMap` arm nodes.
pub fn flatten_case_arms(ast: &Ast, node: NodeIndex, out: &mut Vec<NodeIndex>) {
    if node.is_null() {
        return;
    }
    match ast.node(node).kind {
        ast::NodeKind::CaseAlternative => {
            let children = ast.fixed_children(node);
            if let Some(&left) = children.first() {
                flatten_case_arms(ast, left, out);
            }
            if let Some(&right) = children.get(1) {
                flatten_case_arms(ast, right, out);
            }
        }
        ast::NodeKind::CaseMap => out.push(node),
        _ => {}
    }
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

    #[test]
    fn builtin_views_resolve_from_local_type_pool_without_root_exposure() {
        let src = "typealias MyI64 = .i64'builtin; typealias MyDisplay = .Display'builtin; typealias MyStep = .IterationStep'builtin";
        let (tokens, _) = lexer::tokenize(src);
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let sf = sm.new_source_file(rustc_span::FileName::Custom("t".into()), src.to_string());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let parser = parser::Parser::new(&tokens, src, &diag, sf.start_pos);
        let ast = parser.parse();
        let resolved = resolve_with_options(
            ast,
            &diag,
            ResolveOptions {
                builtin_access: true,
                expose_root_builtins: false,
                expose_root_types: false,
                ..ResolveOptions::default()
            },
        );
        assert!(!diag.has_errors(), "{:?}", diag.diagnostics());
        let my_display = resolved
            .symbols
            .iter()
            .find(|s| str_interner::get(s.name) == "MyDisplay")
            .expect("MyDisplay symbol");
        assert_eq!(
            resolved.type_pool.canonical_type(my_display.type_index),
            Some(resolved.type_pool.well_known.display)
        );
        let my_step = resolved
            .symbols
            .iter()
            .find(|s| str_interner::get(s.name) == "MyStep")
            .expect("MyStep symbol");
        assert_eq!(
            my_step.kind,
            SymbolKind::TypeFactory(TypeFactory::IterationStep)
        );

        let bad_src = "typealias BadClosure = .Closure'builtin";
        let (bad_tokens, _) = lexer::tokenize(bad_src);
        let bad_sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let bad_sf =
            bad_sm.new_source_file(rustc_span::FileName::Custom("bad".into()), bad_src.into());
        let bad_diag = diagnostic::DiagnosticContext::new(&bad_sm);
        let bad_ast =
            parser::Parser::new(&bad_tokens, bad_src, &bad_diag, bad_sf.start_pos).parse();
        let _ = resolve_with_options(bad_ast, &bad_diag, ResolveOptions::for_builtin_package());
        assert!(
            bad_diag.has_errors(),
            "Closure must not be exposed via 'builtin"
        );
    }

    fn parse_and_resolve(source: &str) -> (ResolvedAst, Vec<Diagnostic>, bool, bool) {
        let (tokens, lex_errs) = lexer::tokenize(source);
        assert!(lex_errs.is_empty(), "{source}: {lex_errs:?}");
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let sf = sm.new_source_file(
            rustc_span::FileName::Custom("wave1.ns".into()),
            source.to_string(),
        );
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let ast = parser::Parser::new(&tokens, source, &diag, sf.start_pos).parse();
        let has_parse_errs = diag.has_errors();
        assert!(
            !has_parse_errs,
            "parse error in `{source}`: {:?}",
            diag.diagnostics()
        );
        let resolved = resolve(ast, &diag);
        let has_errors = diag.has_errors();
        let has_warnings = diag.has_warnings();
        let all = diag.diagnostics().to_vec();
        (resolved, all, has_errors, has_warnings)
    }

    #[test]
    fn step2_rejects_unsupported_surface_constructs_with_clear_diagnostics() {
        for (src, needle) in [
            (
                "fn main() { reset p { 1 } }",
                "`reset` expressions are not supported",
            ),
            (
                "fn main() { shift p, k { 1 } }",
                "`shift` expressions are not supported",
            ),
            (
                "fn main() { let s = .foo; s }",
                "standalone symbol literals `.name` are not supported",
            ),
            (
                "fn main() { let o = { a: 1 }; o }",
                "anonymous Object literals `{ ... }` are not supported",
            ),
            (
                "fn main() { 1 match { { x } => x } }",
                "anonymous record patterns `{ ... }` require a struct type name",
            ),
        ] {
            let (_, diags, has_errors, _) = parse_and_resolve(src);
            assert!(has_errors, "expected error for `{src}`, got {diags:?}");
            assert!(
                diags.iter().any(|d| d.message.contains(needle)),
                "expected `{needle}` for `{src}`, got {diags:?}"
            );
        }

        // `.await` via Ast::builder
        let mut ast = Ast::new();
        let lit = ast
            .builder(NodeKind::Int, rustc_span::DUMMY_SP)
            .set_str_id(str_interner::intern("1"))
            .build();
        let aw = ast
            .builder(NodeKind::Await, rustc_span::DUMMY_SP)
            .add_child(lit)
            .build();
        let root = ast
            .builder(NodeKind::FileScope, rustc_span::DUMMY_SP)
            .add_multi_children(&[aw])
            .build();
        ast.root = root;
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let _ = resolve(ast, &diag);
        assert!(diag.has_errors());
        assert!(
            diag.diagnostics()
                .iter()
                .any(|d| d.message.contains("`.await` requires a `Future`")),
            "{:?}",
            diag.diagnostics()
        );
    }

    #[test]
    fn step3_when_statement_types_arms_and_validates_else_and_conditions() {
        let (resolved, diags, has_errors, _) = parse_and_resolve(
            "fn classify(n: i64) -> i64 { when { n < 0 => -1; n == 0 => 0; else => 1 } }",
        );
        assert!(!has_errors, "{diags:?}");
        let fn_sym = resolved
            .symbols
            .iter()
            .find(|s| str_interner::get(s.name) == "classify")
            .unwrap();
        assert!(matches!(
            resolved.type_pool.get(fn_sym.type_index).kind,
            type_pool::TypeKind::Function { ret, .. } if ret == Intrinsic::I64.type_index()
        ));

        // Without `else`, result type is Unit; non-bool condition must fail.
        let (_, _, err_non_bool, _) =
            parse_and_resolve("fn bad() { when { 123 => 1; else => 2 } }");
        assert!(err_non_bool, "non-bool condition in `when` must fail");

        // Duplicate `else` via Ast::builder
        let mut ast = Ast::new();
        let b1 = ast
            .builder(NodeKind::Int, rustc_span::DUMMY_SP)
            .set_str_id(str_interner::intern("1"))
            .build();
        let b2 = ast
            .builder(NodeKind::Int, rustc_span::DUMMY_SP)
            .set_str_id(str_interner::intern("2"))
            .build();
        let e1 = ast
            .builder(NodeKind::ElseConditionArm, rustc_span::DUMMY_SP)
            .add_child(b1)
            .build();
        let e2 = ast
            .builder(NodeKind::ElseConditionArm, rustc_span::DUMMY_SP)
            .add_child(b2)
            .build();
        let w = ast
            .builder(NodeKind::WhenStatement, rustc_span::DUMMY_SP)
            .add_multi_children(&[e1, e2])
            .build();
        ast.root = ast
            .builder(NodeKind::FileScope, rustc_span::DUMMY_SP)
            .add_multi_children(&[w])
            .build();
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let _ = resolve(ast, &diag);
        assert!(diag.has_errors());
    }

    #[test]
    fn step4_pipeline_and_infix_fn_call_resolve_and_typecheck() {
        let (resolved, diags, has_errors, _) = parse_and_resolve(
            "fn add(a: i64, b: i64, .scale: i64 = 1) -> i64 { (a + b) * scale }\n\
             fn inc(x: i64) -> i64 { x + 1 }\n\
             fn main() -> i64 { 10 |> add(5, scale = 2) |> inc }",
        );
        assert!(!has_errors, "{diags:?}");
        assert!(!resolved.pipeline_calls.is_empty());

        // InfixFnCall (`10 plus 32`) works with both parser snippet fallback and explicit str_id
        let (resolved_infix, diags_infix, err_infix, _) = parse_and_resolve(
            "fn plus(a: i64, b: i64) -> i64 { a + b }\nfn main() -> i64 { 10 plus 32 }",
        );
        assert!(!err_infix, "{diags_infix:?}");
        let infix_node = resolved_infix
            .ast
            .nodes
            .iter()
            .enumerate()
            .find_map(|(i, n)| (n.kind == NodeKind::InfixFnCall).then_some(NodeIndex(i as u32)))
            .expect("InfixFnCall node");
        assert!(resolved_infix.node_symbols.contains_key(&infix_node));
        assert_eq!(
            resolved_infix.node_types.get(&infix_node).copied(),
            Some(Intrinsic::I64.type_index())
        );
    }

    #[test]
    fn step5_case_closure_supports_contextual_and_standalone_typing() {
        let (_, diags, has_errors, _) = parse_and_resolve(
            "fn apply_opt(v: ?i64, f: fn(?i64) -> i64) -> i64 { f(v) }\n\
             fn main() -> i64 {\n\
                 let offset = 10\n\
                 let inc = case (x: i64) => x + offset\n\
                 let unwrap_or_zero: fn(?i64) -> i64 = case x? => inc(x) | case null => 0\n\
                 apply_opt(32, unwrap_or_zero)\n\
             }",
        );
        assert!(!has_errors, "{diags:?}");
    }

    #[test]
    fn step6_defer_rejects_exits_across_boundary_but_allows_inner_loops() {
        let (_, diags, has_errors, _) = parse_and_resolve(
            "fn ok() -> i64 {\n\
                 var acc = 0\n\
                 defer { while true { acc += 1; break } }\n\
                 acc\n\
             }",
        );
        assert!(!has_errors, "{diags:?}");

        for bad in [
            "fn bad() -> i64 { defer { return 1 }; 0 }",
            "fn bad() { while true { defer { break }; break } }",
            "fn bad() { while true { defer { continue }; break } }",
        ] {
            let (_, diags, has_errors, _) = parse_and_resolve(bad);
            assert!(has_errors, "expected error for `{bad}`, got {diags:?}");
        }
    }

    #[test]
    fn step7_let_const_var_else_and_module_tuple_destructuring() {
        let (resolved, diags, has_errors, _) = parse_and_resolve(
            "pub const (A, (B, C)) = (10, (20, 12))\n\
             let (X, Y) = (A + B, C)\n\
             fn check(opt: ?i64) -> i64 {\n\
                 let v? = opt else { return -1 }\n\
                 const 42 = v else { return 0 }\n\
                 v + X + Y\n\
             }",
        );
        assert!(!has_errors, "{diags:?}");
        let sym_a = resolved
            .symbols
            .iter()
            .find(|s| str_interner::get(s.name) == "A")
            .unwrap();
        assert_eq!(sym_a.visibility, Visibility::Public);
        assert_eq!(sym_a.kind, SymbolKind::Constant);
        assert_eq!(sym_a.type_index, Intrinsic::I64.type_index());

        // Non-diverging `else` must fail
        let (_, diags_bad, err_bad, _) =
            parse_and_resolve("fn bad(opt: ?i64) -> i64 { let v? = opt else { 0 }; v }");
        assert!(
            err_bad,
            "non-diverging else must be rejected: {diags_bad:?}"
        );
        assert!(
            diags_bad.iter().any(|d| d.message.contains("diverge")),
            "{diags_bad:?}"
        );
    }

    #[test]
    fn step8_struct_patterns_nested_typed_patterns_and_handler_arm_destructuring() {
        let (_, diags, has_errors, _) = parse_and_resolve(
            "struct Point { x: i64, y: i64, z: i64 }\n\
             effect emit(pair: (i64, i64), catch k) -> i64\n\
             fn sum_pair((a, b): (i64, i64)) -> i64 { a + b }\n\
             fn main() -> i64 {\n\
                 let (p, q): (i64, i64) = (3, 4)\n\
                 let pt = Point { x: p, y: q, z: 99 }\n\
                 let base = pt match { Point { x, y: 4, z: _ } => x + 10, _ => 0 }\n\
                 let handled = (emit((base, q))#) # { emit((u, v), k) => k(u + v) }\n\
                 sum_pair((handled, 1))\n\
             }",
        );
        assert!(!has_errors, "{diags:?}");

        // Nested PatternTyped([PatternTuple, TupleType]) inside LetDecl + PatternExtendedCall with `...` (PatternRestBind)
        let mut ast = Ast::new();
        let sp = rustc_span::DUMMY_SP;
        let pt_name = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("Pt"))
            .build();
        let fx_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("x"))
            .build();
        let fx_ty = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let fx = ast
            .builder(NodeKind::StructField, sp)
            .add_child(fx_id)
            .add_child(fx_ty)
            .add_child(NodeIndex::NULL)
            .build();
        let fy_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("y"))
            .build();
        let fy_ty = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let fy = ast
            .builder(NodeKind::StructField, sp)
            .add_child(fy_id)
            .add_child(fy_ty)
            .add_child(NodeIndex::NULL)
            .build();
        let pt_def = ast
            .builder(NodeKind::StructDef, sp)
            .add_child(pt_name)
            .add_multi_children(&[fx, fy])
            .build();

        let a_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("a"))
            .build();
        let b_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("b"))
            .build();
        let pat_tup = ast
            .builder(NodeKind::PatternTuple, sp)
            .add_multi_children(&[a_id, b_id])
            .build();
        let t_i64_1 = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let t_i64_2 = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let tup_ty = ast
            .builder(NodeKind::Tuple, sp)
            .add_multi_children(&[t_i64_1, t_i64_2])
            .build();
        let pat_typed = ast
            .builder(NodeKind::PatternTyped, sp)
            .add_child(pat_tup)
            .add_child(tup_ty)
            .build();
        let c3 = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("3"))
            .build();
        let c4 = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("4"))
            .build();
        let val_tup = ast
            .builder(NodeKind::Tuple, sp)
            .add_multi_children(&[c3, c4])
            .build();
        let let_typed = ast
            .builder(NodeKind::LetDecl, sp)
            .add_child(pat_typed)
            .add_child(NodeIndex::NULL)
            .add_child(val_tup)
            .add_child(NodeIndex::NULL)
            .build();

        // `let Pt { x, ... } = p`
        let p_param_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("p"))
            .build();
        let p_param_ty = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("Pt"))
            .build();
        let p_param = ast
            .builder(NodeKind::ParamTyped, sp)
            .add_child(p_param_id)
            .add_child(p_param_ty)
            .build();
        let pt_pat_callee = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("Pt"))
            .build();
        let x_short = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("x"))
            .build();
        let rest_ignore = ast
            .builder(NodeKind::PatternRestBind, sp)
            .add_child(NodeIndex::NULL)
            .build();
        let pt_pat = ast
            .builder(NodeKind::PatternExtendedCall, sp)
            .add_child(pt_pat_callee)
            .add_multi_children(&[x_short, rest_ignore])
            .build();
        let p_ref = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("p"))
            .build();
        let let_pt = ast
            .builder(NodeKind::LetDecl, sp)
            .add_child(pt_pat)
            .add_child(NodeIndex::NULL)
            .add_child(p_ref)
            .add_child(NodeIndex::NULL)
            .build();

        let fn_name = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("f"))
            .build();
        let a_ref = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("a"))
            .build();
        let x_ref = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("x"))
            .build();
        let sum_ax = ast
            .builder(NodeKind::Add, sp)
            .add_child(a_ref)
            .add_child(x_ref)
            .build();
        let blk = ast
            .builder(NodeKind::Block, sp)
            .add_multi_children(&[let_typed, let_pt, sum_ax])
            .build();
        let fn_def = ast
            .builder(NodeKind::FunctionDef, sp)
            .add_child(fn_name)
            .add_child(NodeIndex::NULL)
            .add_child(blk)
            .add_child(NodeIndex::NULL)
            .add_multi_children(&[p_param])
            .build();
        ast.root = ast
            .builder(NodeKind::FileScope, sp)
            .add_multi_children(&[pt_def, fn_def])
            .build();
        ast.source = Some("".into());
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let resolved_nested = resolve(ast, &diag);
        assert!(!diag.has_errors(), "{:?}", diag.diagnostics());
        assert!(
            resolved_nested
                .annotation_typed_patterns
                .contains(&pat_typed)
        );
        assert!(resolved_nested.struct_patterns.contains_key(&pt_pat));

        // Missing field without `...` and private field outside module must fail
        for (bad, needle) in [
            (
                "struct Point { x: i64, y: i64 }\nfn main(p: Point) { p match { Point { x } => x } }",
                "missing struct field `y`",
            ),
            (
                "mod m { pub struct S { pub a: i64, private b: i64 } }\nfn main(s: m.S) { s match { m.S { a, b } => a + b } }",
                "not visible",
            ),
        ] {
            let (_, diags, has_errors, _) = parse_and_resolve(bad);
            assert!(has_errors, "expected error for `{bad}`, got {diags:?}");
            assert!(
                diags.iter().any(|d| d.message.contains(needle)),
                "expected `{needle}` in {diags:?}"
            );
        }
    }

    #[test]
    fn step9_error_unwrap_bound_methods_spread_args_and_exhaustiveness_warnings() {
        // 1. `!E T` `.unwrap()` + bound method values on structs and enums
        let (_, diags, has_errors, _) = parse_and_resolve(
            "enum Err { Fail }\n\
             struct Counter { base: i64 }\n\
             impl Counter { fn add(self, delta: i64) -> i64 { self.base + delta } }\n\
             enum Mode { fast, slow, fn weight(self) -> i64 { self match { Mode.fast => 10, Mode.slow => 1 } } }\n\
             fn compute(ok: bool) -> !Err i64 { if ok { 32 } else { error Err.Fail } }\n\
             fn main() -> i64 {\n\
                 let c = Counter { base: 10 }\n\
                 let f: fn(i64) -> i64 = c.add\n\
                 let w: fn() -> i64 = Mode.fast.weight\n\
                 f(compute(true).unwrap()) + w()\n\
             }",
        );
        assert!(!has_errors, "{diags:?}");

        // 2. Exhaustiveness warnings: `Level::Warning` WITHOUT setting `has_errors`!
        for (src, missing_part) in [
            (
                "enum Color { red, green, blue }\nfn f(c: Color) -> i64 { c match { Color.red => 1, Color.green => 2 } }",
                "Color.blue",
            ),
            ("fn f(b: bool) -> i64 { b match { true => 1 } }", "false"),
            ("fn f(x: ?i64) -> i64 { x match { v? => v } }", "null"),
        ] {
            let (_, diags, has_errors, has_warnings) = parse_and_resolve(src);
            assert!(
                !has_errors,
                "non-exhaustive match must NOT be a hard error: {diags:?}"
            );
            assert!(has_warnings, "expected exhaustiveness warning for `{src}`");
            assert!(
                diags.iter().any(|d| {
                    d.level == diagnostic::Level::Warning && d.message.contains(missing_part)
                }),
                "expected warning mentioning `{missing_part}`, got {diags:?}"
            );
        }

        // Exhaustive matches emit NO warnings
        for src in [
            "enum Color { red, green }\nfn f(c: Color) -> i64 { c match { Color.red or Color.green => 1 } }",
            "fn f(b: bool) -> i64 { b match { true => 1, false => 0 } }",
            "fn f(x: ?i64) -> i64 { x match { v? => v, null => 0 } }",
        ] {
            let (_, diags, has_errors, has_warnings) = parse_and_resolve(src);
            assert!(!has_errors, "{diags:?}");
            assert!(
                !has_warnings,
                "unexpected warning on exhaustive `{src}`: {diags:?}"
            );
        }

        // 3. Call-site `SpreadArg` via Ast::builder (valid in Call, rejected outside Call or with non-List)
        let (mut base_resolved, _, base_err, _) = parse_and_resolve(
            "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() -> i64 { let xs = [20, 22]; add(1, 2) }",
        );
        assert!(!base_err);
        let mut ast = std::mem::take(&mut base_resolved.ast);
        let call_idx = ast
            .nodes
            .iter()
            .enumerate()
            .find_map(|(i, n)| {
                (n.kind == NodeKind::Call
                    && str_interner::get(ast.node(n.children[0]).str_id) == "add")
                    .then_some(NodeIndex(i as u32))
            })
            .unwrap();
        let xs_id = ast
            .builder(NodeKind::Id, rustc_span::DUMMY_SP)
            .set_str_id(str_interner::intern("xs"))
            .build();
        let spread = ast
            .builder(NodeKind::SpreadArg, rustc_span::DUMMY_SP)
            .add_child(xs_id)
            .build();
        let callee = ast.fixed_children(call_idx)[0];
        let new_call = ast
            .builder(NodeKind::Call, rustc_span::DUMMY_SP)
            .add_child(callee)
            .add_multi_children(&[spread])
            .build();
        // Replace old call in main's block
        let main_fn = ast.multi_children(ast.root)[1];
        let main_block = ast.fixed_children(main_fn)[2];
        let let_stmt = ast.multi_children(main_block)[0];
        let new_block = ast
            .builder(NodeKind::Block, rustc_span::DUMMY_SP)
            .add_multi_children(&[let_stmt, new_call])
            .build();
        ast.nodes[main_fn.0 as usize].children[2] = new_block;
        let sm = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let resolved_spread = resolve(ast, &diag);
        assert!(!diag.has_errors(), "{:?}", diag.diagnostics());
        assert!(
            resolved_spread
                .call_arguments
                .get(&new_call)
                .and_then(|p| p.spread_pack.as_ref())
                .is_some_and(|pack| pack.fixed_count == 2 && !pack.has_variadic_tail)
        );
    }
}
