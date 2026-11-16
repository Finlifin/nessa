mod arguments;
mod builder;
mod capability;
mod characters;
mod collections;
mod compound;
mod concat;
mod construction;
mod continuation;
mod defaults;
mod derivation;
mod display_derivation;
mod display_text;
mod dynamic_equality;
mod dynamic_loops;
mod enums;
mod errors;
mod expr;
mod hash_derivation;
mod initialization;
mod json_derivation;
mod list_patterns;
mod lists;
mod literal;
mod lowering;
mod named_functions;
mod optional;
mod ordering;
mod ordering_derivation;
mod patterns;
mod self_specialization;
mod stmt;
mod trait_dispatch;
mod trait_parameters;
mod tuples;

use nsbc::{FuncId, GlobalId, GlobalInfo};
use resolution::ResolvedAst;
use runtime::BuiltinFnId;
use str_interner::StrId;
use type_pool::TypeIndex;

// ---------------------------------------------------------------------------
// BlockId and NirLocal — basic identifiers
// ---------------------------------------------------------------------------

/// Identifies a basic block within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// A local variable / temporary in NIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NirLocal(pub u32);

// ---------------------------------------------------------------------------
// NirValue — operand in expressions
// ---------------------------------------------------------------------------

/// An operand that can be used in NIR expressions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NirValue {
    /// A local variable reference.
    Local(NirLocal),
    /// An i64 constant.
    ConstInt(i64),
    /// A u64 constant.
    ConstUInt(u64),
    /// An i128 constant, including small values with an explicit i128 type.
    ConstI128(i128),
    /// A u128 constant, including small values with an explicit u128 type.
    ConstU128(u128),
    /// An f64 constant.
    ConstFloat(f64),
    /// A boolean constant.
    ConstBool(bool),
    /// A Unicode scalar value, distinct from a one-character string.
    ConstChar(char),
    /// A string constant (interned).
    ConstStr(StrId),
    /// An immutable pool-local type descriptor.
    ConstType(TypeIndex),
    /// A nullary member with nominal enum identity.
    ConstEnum(TypeIndex, u32),
    /// Unit value.
    Unit,
    /// Null value.
    Null,
}

// ---------------------------------------------------------------------------
// BinOp / UnaryOp — arithmetic and logic operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Eager conjunction of boolean values; source `and` lowers to a CFG.
    And,
    /// Eager disjunction of boolean values; source `or` lowers to a CFG.
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

// ---------------------------------------------------------------------------
// NirExpr — expressions in NIR
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NirExpr {
    /// Original lexical context of a dynamic call, independent of function scope.
    ScopedCall {
        scope: u32,
        call: Box<NirExpr>,
    },
    /// Construct an enum from an already rooted tuple of declaration-order fields.
    NewEnum(TypeIndex, u32, NirValue),
    /// Test nominal enum identity and variant before extracting any payload.
    EnumIs(NirValue, TypeIndex, u32),
    EnumField(NirValue, u32),
    ErrorOk(NirValue, TypeIndex),
    ErrorErr(NirValue, TypeIndex),
    ErrorIsOk(NirValue),
    ErrorPayload(NirValue),
    /// A value (local or constant).
    Use(NirValue),
    /// Read a shared module value, rejecting reads before initialization.
    LoadGlobal(GlobalId),
    /// Binary operation.
    BinOp(BinOp, NirValue, NirValue),
    /// Unary operation.
    UnaryOp(UnaryOp, NirValue),
    /// Function call.
    Call(FuncId, Vec<NirValue>),
    CallWithProof(FuncId, Vec<NirValue>),
    TraitProof(NirValue, TypeIndex),
    TraitAssert(NirValue, NirValue, TypeIndex),
    TraitProject(NirValue, TypeIndex),
    TraitCall {
        receiver: NirValue,
        proof: NirValue,
        view: TypeIndex,
        slot: u32,
        args: Vec<NirValue>,
    },
    /// Builtin function call (native VM implementation via CallBuiltin).
    CallBuiltin(BuiltinFnId, Vec<NirValue>),
    /// Invoke an effect through the nearest dynamically installed handler.
    EffectCall(TypeIndex, Vec<NirValue>),
    DelimitedCall {
        body: NirValue,
        handler_count: u8,
    },
    ResumeContinuation(NirValue, NirValue),
    /// Consume a fresh handler binding whose sole reference is this tail call.
    ResumeContinuationOnce(NirValue, NirValue),
    /// Await completion of a Future value.
    Await(NirValue),
    /// Method call: receiver, method name, args.
    MethodCall(NirValue, StrId, Vec<NirValue>),
    /// Field access: object, field index.
    FieldAccess(NirValue, u32),
    /// Index access: object, index.
    IndexAccess(NirValue, NirValue),
    /// Allocate a new object of the given type with field values.
    NewObject(TypeIndex, Vec<NirValue>),
    /// Allocate a List with the given initial length and Unit elements.
    NewList(u16),
    /// Create a closure: function id, captured values.
    NewClosure(FuncId, Vec<NirValue>),
    /// Indirect call through a closure value: callee, args.
    CallIndirect(NirValue, Vec<NirValue>),
    CallIndirectProof(NirValue, Vec<NirValue>),
    /// Runtime type check.
    TypeCheck(NirValue, TypeIndex),
    /// Type cast.
    TypeCast(NirValue, TypeIndex),
    /// Gradual boundary check, allowing widening but never numeric narrowing.
    TypeAssert(NirValue, TypeIndex),
}

impl NirExpr {
    /// Whether emission can perform a lexical method or trait type query.
    pub fn requires_scope(&self) -> bool {
        matches!(
            self,
            Self::TraitProof(..)
                | Self::TraitAssert(..)
                | Self::TraitCall { .. }
                | Self::CallIndirectProof(..)
                | Self::MethodCall(..)
                | Self::CallIndirect(..)
                | Self::TypeCheck(..)
                | Self::TypeCast(..)
                | Self::TypeAssert(..)
                | Self::FieldAccess(..)
                | Self::NewObject(..)
                | Self::NewEnum(..)
                | Self::EnumField(..)
                | Self::ErrorOk(..)
                | Self::ErrorErr(..)
                | Self::ErrorIsOk(..)
                | Self::ErrorPayload(..)
        )
    }

    /// Attach the original lexical context to an expression with checked types.
    pub fn in_scope(self, scope: u32) -> Self {
        Self::ScopedCall {
            scope,
            call: Box::new(self),
        }
    }

    pub(crate) fn in_source_scope(
        self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Self {
        match resolved.node_scopes.get(&node) {
            Some(scope) => self.in_scope(scope.0),
            None => self, // Checked codegen diagnoses incomplete external metadata.
        }
    }

    /// Inspect the underlying expression while retaining scope metadata.
    pub fn unscoped(&self) -> &Self {
        match self {
            Self::ScopedCall { call, .. } => call,
            expression => expression,
        }
    }
}

// ---------------------------------------------------------------------------
// NirStmt — statements in a basic block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NirStmt {
    /// Context of a store that can assert a declared trait type.
    Scoped {
        scope: u32,
        statement: Box<NirStmt>,
    },
    /// Checked list update: receiver, integer index, new value.
    StoreIndex(NirValue, NirValue, NirValue),
    /// Write a checked declared field of an object or tuple.
    StoreField(NirValue, u32, NirValue),
    /// Assign an expression to a local.
    Assign(NirLocal, NirExpr),
    /// Initialize or update a shared module value.
    StoreGlobal(GlobalId, NirValue),
    PushHandler {
        effect: TypeIndex,
        closure: NirValue,
        continuation_param: Option<u8>,
    },
    PopHandler,
    /// Await all pending child tasks spawned under the top handler scope, then pop it.
    AwaitPopHandler,
    /// No operation.
    Nop,
}

impl NirStmt {
    pub fn requires_scope(&self) -> bool {
        matches!(self, Self::StoreGlobal(..) | Self::StoreField(..))
    }

    pub fn in_scope(self, scope: u32) -> Self {
        Self::Scoped {
            scope,
            statement: Box::new(self),
        }
    }

    pub(crate) fn in_source_scope(
        self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Self {
        match resolved.node_scopes.get(&node) {
            Some(scope) => self.in_scope(scope.0),
            None => self,
        }
    }

    pub fn unscoped(&self) -> &Self {
        match self {
            Self::Scoped { statement, .. } => statement,
            statement => statement,
        }
    }
}

// ---------------------------------------------------------------------------
// Terminator — how a basic block ends
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Terminator {
    /// Unconditional jump.
    Goto(BlockId),
    /// Conditional branch: condition, then-block, else-block.
    Branch(NirValue, BlockId, BlockId),
    /// Return from function.
    Return(NirValue),
    /// No pattern matched; do not fabricate a result or execute another arm.
    MatchFail,
    /// Unreachable (NoReturn path).
    Unreachable,
}

// ---------------------------------------------------------------------------
// BasicBlock
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub stmts: Vec<NirStmt>,
    pub terminator: Terminator,
}

// ---------------------------------------------------------------------------
// NirParam — function parameter in NIR
// ---------------------------------------------------------------------------

/// Physical parameter role. Captures form a prefix before source parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NirParamRole {
    Capture,
    CaptureProof {
        view: TypeIndex,
    },
    TraitProof {
        view: TypeIndex,
    },
    /// A concrete Self parameter of a generated trait default adapter.
    TraitSelfProof {
        view: TypeIndex,
    },
    User,
}

#[derive(Debug, Clone)]
pub struct NirParam {
    pub local: NirLocal,
    pub name: StrId,
    pub type_index: TypeIndex,
    pub role: NirParamRole,
}

// ---------------------------------------------------------------------------
// NirFunction — a single function in NIR form
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NirFunction {
    /// Declaration context for implicit parameter type assertions.
    pub entry_scope: Option<u32>,
    pub func_id: FuncId,
    pub name: StrId,
    /// User-visible function signature, excluding closure capture parameters.
    /// Internal handler closures without a source function type use INVALID.
    pub function_type: TypeIndex,
    pub display_owner: Option<TypeIndex>,
    pub params: Vec<NirParam>,
    pub return_type: TypeIndex,
    pub blocks: Vec<BasicBlock>,
    pub entry_block: BlockId,
    /// Number of locals allocated (including params).
    pub local_count: u32,
    /// Whether this function is a closure (lambda with captures).
    pub is_closure: bool,
}

impl NirFunction {
    /// Explicit capture prefix and logical parameters, including hidden proofs.
    /// Codegen checks role ordering before publishing this layout.
    pub fn entry_abi(&self) -> nsbc::FunctionAbi {
        let mut captures = Vec::new();
        let mut parameters = Vec::new();
        let mut trait_data = false;
        for parameter in &self.params {
            match parameter.role {
                NirParamRole::Capture => captures.push(nsbc::CaptureAbi::Value),
                NirParamRole::CaptureProof { view } => {
                    captures.push(nsbc::CaptureAbi::TraitProof { view })
                }
                NirParamRole::TraitProof { view } => {
                    parameters.push(nsbc::ParameterAbi::Trait { view });
                    trait_data = true;
                }
                NirParamRole::TraitSelfProof { view } => {
                    parameters.push(nsbc::ParameterAbi::TraitSelf { view });
                    trait_data = true;
                }
                NirParamRole::User if trait_data => trait_data = false,
                NirParamRole::User => parameters.push(nsbc::ParameterAbi::Value),
            }
        }
        nsbc::FunctionAbi {
            captures,
            parameters,
        }
    }
}

// ---------------------------------------------------------------------------
// NirModule — the complete NIR for a compilation unit
// ---------------------------------------------------------------------------

/// A compiler-generated method whose identity must be published after source relocation.
#[derive(Debug, Clone, Copy)]
pub struct DerivedMethod {
    pub implementor: TypeIndex,
    pub trait_type: TypeIndex,
    pub method_name: StrId,
    pub func_id: FuncId,
}

#[derive(Debug)]
pub struct NirModule {
    pub derived_methods: Vec<DerivedMethod>,
    /// Source function symbols relocated to their generated code identities.
    pub function_symbols: std::collections::HashMap<resolution::SymbolId, FuncId>,
    pub functions: Vec<NirFunction>,
    pub globals: Vec<GlobalInfo>,
    /// Startup function that initializes loaded modules and invokes user main.
    pub entry: Option<FuncId>,
}

// ---------------------------------------------------------------------------
// Convenience entry point
// ---------------------------------------------------------------------------

/// Lower a resolved AST whose initialization dependencies are already valid.
///
/// # Panics
/// Panics on invalid module initialization. Source compilation should use
/// [`try_lower`] to report these errors with their source locations.
pub fn lower(resolved: &ResolvedAst) -> NirModule {
    try_lower(resolved).expect("resolved initialization dependencies must be valid")
}

/// A source error found while planning module startup.
#[derive(Debug)]
pub struct LoweringError {
    pub node: ast::NodeIndex,
    pub message: String,
}

/// Whether startup invokes the root main after module initialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupMode {
    Main,
    /// Executable packages require a local, zero-argument root main.
    RequiredMain,
    InitializationOnly,
}

/// Lower source with checked module initialization dependencies.
pub fn try_lower(resolved: &ResolvedAst) -> Result<NirModule, Vec<LoweringError>> {
    try_lower_with_startup(resolved, StartupMode::Main)
}

pub fn try_lower_with_startup(
    resolved: &ResolvedAst,
    startup: StartupMode,
) -> Result<NirModule, Vec<LoweringError>> {
    if startup == StartupMode::RequiredMain {
        let main = root_main(resolved).ok_or_else(|| {
            vec![LoweringError {
                node: resolved.ast.root,
                message: "exe package must define a root main function".into(),
            }]
        })?;
        let node = resolved.symbols[main.0 as usize].def_node;
        if !resolved.ast.multi_children(node).is_empty() {
            return Err(vec![LoweringError {
                node,
                message: "exe main must have zero parameters".into(),
            }]);
        }
    }
    let initialization = initialization::InitializationPlan::build(resolved)?;
    named_functions::validate(resolved, &initialization.slots)?;
    Ok(lowering::NirLowering::new().lower(resolved, initialization, startup))
}

/// Select the local root function, excluding imported and nested mains.
fn root_main(resolved: &ResolvedAst) -> Option<resolution::SymbolId> {
    let scope = resolved
        .scopes
        .iter()
        .find(|scope| scope.node == resolved.ast.root)?;
    scope
        .bindings
        .get(&str_interner::intern("main"))
        .copied()
        .filter(|&id| {
            let symbol = &resolved.symbols[id.0 as usize];
            symbol.kind == resolution::SymbolKind::Function && symbol.scope == scope.id
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ast::{Ast, NodeIndex, NodeKind};

    fn make_simple_func_ast() -> Ast {
        // Build: fn foo() { return 42 }
        let mut ast = Ast::new();
        ast.source = Some("fn foo(){return 42}".into());
        let foo_str = str_interner::intern("foo");

        // Int literal
        let int_node = {
            let mut b = ast.builder(NodeKind::Int, rustc_span::DUMMY_SP);
            b.set_str_id(str_interner::intern("42"));
            b.build()
        };

        // return statement
        let ret_node = {
            let mut b = ast.builder(NodeKind::ReturnStatement, rustc_span::DUMMY_SP);
            b.add_child(int_node);
            b.add_child(NodeIndex::NULL); // no guard
            b.build()
        };

        // block { return 42 }
        let block_node = {
            let mut b = ast.builder(NodeKind::Block, rustc_span::DUMMY_SP);
            b.add_multi_children(&[ret_node]);
            b.build()
        };

        // fn foo()
        let name_node = {
            let mut b = ast.builder(NodeKind::Id, rustc_span::DUMMY_SP);
            b.set_str_id(foo_str);
            b.build()
        };

        let func_node = {
            let mut b = ast.builder(NodeKind::FunctionDef, rustc_span::DUMMY_SP);
            b.add_child(name_node); // [0] name
            b.add_child(NodeIndex::NULL); // [1] return type
            b.add_child(block_node); // [2] body
            b.add_child(NodeIndex::NULL); // [3] capability
            b.build()
        };

        let root = {
            let mut b = ast.builder(NodeKind::FileScope, rustc_span::DUMMY_SP);
            b.add_multi_children(&[func_node]);
            b.build()
        };
        ast.root = root;
        ast
    }

    #[test]
    fn lower_simple_function() {
        let ast = make_simple_func_ast();
        let sm = rustc_span::source_map::SourceMap::new(
            rustc_span::source_map::FilePathMapping::empty(),
        );
        let diag_ctx = diagnostic::DiagnosticContext::new(&sm);
        let resolved = resolution::resolve(ast, &diag_ctx);
        let nir = lower(&resolved);
        assert_eq!(nir.functions.len(), 1);
        let func = &nir.functions[0];
        assert!(!func.blocks.is_empty(), "should have at least one block");
        assert!(
            func.blocks.iter().any(|block| matches!(
                block.terminator,
                Terminator::Return(NirValue::ConstInt(42))
            ))
        );
    }

    #[test]
    fn entry_block_has_terminator() {
        let ast = make_simple_func_ast();
        let sm2 = rustc_span::source_map::SourceMap::new(
            rustc_span::source_map::FilePathMapping::empty(),
        );
        let diag_ctx2 = diagnostic::DiagnosticContext::new(&sm2);
        let resolved = resolution::resolve(ast, &diag_ctx2);
        let nir = lower(&resolved);
        let func = &nir.functions[0];
        let entry = &func.blocks[func.entry_block.0 as usize];
        assert!(
            !matches!(entry.terminator, Terminator::Unreachable),
            "entry block should have a valid terminator"
        );
    }

    #[test]
    fn entry_uses_the_file_scope_and_excludes_imported_main() {
        let mut ast = make_simple_func_ast();
        let function = ast.multi_children(ast.root)[0];
        let name = ast.fixed_children(function)[0];
        ast.nodes[name.0 as usize].str_id = str_interner::intern("main");
        ast.source = Some("fn main(){return 42}".into());
        let source_map = rustc_span::source_map::SourceMap::new(
            rustc_span::source_map::FilePathMapping::empty(),
        );
        let diagnostics = diagnostic::DiagnosticContext::new(&source_map);
        let mut resolved = resolution::resolve(ast, &diagnostics);
        let symbol = resolved.node_symbols[&name];
        let file_scope = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        assert_ne!(file_scope.id, resolution::ScopeId::ROOT);
        assert_eq!(lower(&resolved).entry, Some(FuncId(0)));

        // An imported alias can be bound as main in the file scope, but its
        // defining scope still belongs to the library.
        resolved.symbols[symbol.0 as usize].scope = resolution::ScopeId::ROOT;
        assert_eq!(lower(&resolved).entry, None);
    }

    #[test]
    fn lower_when_defer_var_else_infix_bound_method_and_spread() {
        let sp = rustc_span::DUMMY_SP;
        let mut ast = Ast::new();

        // struct Boxed { val: i64 }
        // impl Boxed { fn add(self, d: i64) -> i64 { self.val + d } }
        let boxed_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("Boxed"))
            .build();
        let val_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("val"))
            .build();
        let i64_ty1 = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let field_def = ast
            .builder(NodeKind::StructField, sp)
            .add_child(val_id)
            .add_child(i64_ty1)
            .add_child(NodeIndex::NULL)
            .build();
        let struct_def = ast
            .builder(NodeKind::StructDef, sp)
            .add_child(boxed_id)
            .add_multi_children(&[field_def])
            .build();

        let impl_target = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("Boxed"))
            .build();
        let add_name = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("add"))
            .build();
        let param_self = ast.builder(NodeKind::ParamSelf, sp).build();
        let d_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("d"))
            .build();
        let i64_ty2 = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let param_d = ast
            .builder(NodeKind::ParamTyped, sp)
            .add_child(d_id)
            .add_child(i64_ty2)
            .build();
        let i64_ret = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let self_ref = ast
            .builder(NodeKind::SelfLower, sp)
            .set_str_id(str_interner::intern("self"))
            .build();
        let val_mem = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("val"))
            .build();
        let self_val = ast
            .builder(NodeKind::Projection, sp)
            .add_child(self_ref)
            .add_child(val_mem)
            .build();
        let d_ref = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("d"))
            .build();
        let sum_expr = ast
            .builder(NodeKind::Add, sp)
            .add_child(self_val)
            .add_child(d_ref)
            .build();
        let add_body = ast
            .builder(NodeKind::Block, sp)
            .add_multi_children(&[sum_expr])
            .build();
        let method_def = ast
            .builder(NodeKind::FunctionDef, sp)
            .add_child(add_name)
            .add_child(i64_ret)
            .add_child(add_body)
            .add_child(NodeIndex::NULL)
            .add_multi_children(&[param_self, param_d])
            .build();
        let impl_def = ast
            .builder(NodeKind::ImplDef, sp)
            .add_child(impl_target)
            .add_multi_children(&[method_def])
            .build();

        // fn plus(a: i64, b: i64) -> i64 { a + b }
        let plus_name = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("plus"))
            .build();
        let pa_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("a"))
            .build();
        let pa_ty = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let pa = ast
            .builder(NodeKind::ParamTyped, sp)
            .add_child(pa_id)
            .add_child(pa_ty)
            .build();
        let pb_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("b"))
            .build();
        let pb_ty = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let pb = ast
            .builder(NodeKind::ParamTyped, sp)
            .add_child(pb_id)
            .add_child(pb_ty)
            .build();
        let plus_ret = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let ra = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("a"))
            .build();
        let rb = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("b"))
            .build();
        let plus_sum = ast
            .builder(NodeKind::Add, sp)
            .add_child(ra)
            .add_child(rb)
            .build();
        let plus_body = ast
            .builder(NodeKind::Block, sp)
            .add_multi_children(&[plus_sum])
            .build();
        let plus_fn = ast
            .builder(NodeKind::FunctionDef, sp)
            .add_child(plus_name)
            .add_child(plus_ret)
            .add_child(plus_body)
            .add_child(NodeIndex::NULL)
            .add_multi_children(&[pa, pb])
            .build();

        // Module-scope tuple destructuring: const (G1, G2) = (10, 20)
        let g1_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("G1"))
            .build();
        let g2_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("G2"))
            .build();
        let g_pat = ast
            .builder(NodeKind::PatternTuple, sp)
            .add_multi_children(&[g1_id, g2_id])
            .build();
        let c10 = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("10"))
            .build();
        let c20 = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("20"))
            .build();
        let g_val = ast
            .builder(NodeKind::Tuple, sp)
            .add_multi_children(&[c10, c20])
            .build();
        let mod_const = ast
            .builder(NodeKind::ConstDecl, sp)
            .add_child(g_pat)
            .add_child(NodeIndex::NULL)
            .add_child(g_val)
            .add_child(NodeIndex::NULL)
            .build();

        // fn main() -> i64:
        //   var 10 = G1 else { return -1 }
        //   defer { () }
        //   let bx = Boxed { val: G1 plus G2 }
        //   let f = bx.add
        //   let args = [12]
        //   when { true => f(...args), else => 0 }
        let pat_10 = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("10"))
            .build();
        let ref_g1 = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("G1"))
            .build();
        let ret_zero = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("0"))
            .build();
        let else_ret = ast
            .builder(NodeKind::ReturnStatement, sp)
            .add_child(ret_zero)
            .add_child(NodeIndex::NULL)
            .build();
        let else_blk = ast
            .builder(NodeKind::Block, sp)
            .add_multi_children(&[else_ret])
            .build();
        let var_else = ast
            .builder(NodeKind::VarDecl, sp)
            .add_child(pat_10)
            .add_child(NodeIndex::NULL)
            .add_child(ref_g1)
            .add_child(else_blk)
            .build();

        let unit_node = ast.builder(NodeKind::Unit, sp).build();
        let defer_blk = ast
            .builder(NodeKind::Block, sp)
            .add_multi_children(&[unit_node])
            .build();
        let defer_stmt = ast
            .builder(NodeKind::DeferStatement, sp)
            .add_child(defer_blk)
            .build();

        let lhs_g1 = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("G1"))
            .build();
        let rhs_g2 = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("G2"))
            .build();
        let infix_call = ast
            .builder(NodeKind::InfixFnCall, sp)
            .set_str_id(str_interner::intern("plus"))
            .add_child(lhs_g1)
            .add_child(rhs_g2)
            .build();
        let boxed_ctor = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("Boxed"))
            .build();
        let prop_key = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("val"))
            .build();
        let prop = ast
            .builder(NodeKind::Property, sp)
            .add_child(prop_key)
            .add_child(infix_call)
            .build();
        let ext_ctor = ast
            .builder(NodeKind::ExtendedCall, sp)
            .add_child(boxed_ctor)
            .add_multi_children(&[prop])
            .build();
        let bx_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("bx"))
            .build();
        let let_bx = ast
            .builder(NodeKind::LetDecl, sp)
            .add_child(bx_id)
            .add_child(NodeIndex::NULL)
            .add_child(ext_ctor)
            .add_child(NodeIndex::NULL)
            .build();

        let bx_ref = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("bx"))
            .build();
        let add_mem = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("add"))
            .build();
        let bound_proj = ast
            .builder(NodeKind::Projection, sp)
            .add_child(bx_ref)
            .add_child(add_mem)
            .build();
        let f_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("f"))
            .build();
        let let_f = ast
            .builder(NodeKind::LetDecl, sp)
            .add_child(f_id)
            .add_child(NodeIndex::NULL)
            .add_child(bound_proj)
            .add_child(NodeIndex::NULL)
            .build();

        let c12 = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("12"))
            .build();
        let list_12 = ast
            .builder(NodeKind::ListOf, sp)
            .add_multi_children(&[c12])
            .build();
        let args_id = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("args"))
            .build();
        let let_args = ast
            .builder(NodeKind::LetDecl, sp)
            .add_child(args_id)
            .add_child(NodeIndex::NULL)
            .add_child(list_12)
            .add_child(NodeIndex::NULL)
            .build();

        let f_ref = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("f"))
            .build();
        let args_ref = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("args"))
            .build();
        let spread_args = ast
            .builder(NodeKind::SpreadArg, sp)
            .add_child(args_ref)
            .build();
        let call_f = ast
            .builder(NodeKind::Call, sp)
            .add_child(f_ref)
            .add_multi_children(&[spread_args])
            .build();
        let cond_true = ast
            .builder(NodeKind::Bool, sp)
            .set_str_id(str_interner::intern("true"))
            .build();
        let arm1 = ast
            .builder(NodeKind::ConditionArm, sp)
            .add_child(cond_true)
            .add_child(call_f)
            .build();
        let else_zero = ast
            .builder(NodeKind::Int, sp)
            .set_str_id(str_interner::intern("0"))
            .build();
        let arm_else = ast
            .builder(NodeKind::ElseConditionArm, sp)
            .add_child(else_zero)
            .build();
        let when_expr = ast
            .builder(NodeKind::WhenStatement, sp)
            .add_multi_children(&[arm1, arm_else])
            .build();

        let main_name = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("main"))
            .build();
        let main_ret = ast
            .builder(NodeKind::Id, sp)
            .set_str_id(str_interner::intern("i64"))
            .build();
        let main_blk = ast
            .builder(NodeKind::Block, sp)
            .add_multi_children(&[var_else, defer_stmt, let_bx, let_f, let_args, when_expr])
            .build();
        let main_fn = ast
            .builder(NodeKind::FunctionDef, sp)
            .add_child(main_name)
            .add_child(main_ret)
            .add_child(main_blk)
            .add_child(NodeIndex::NULL)
            .build();

        ast.root = ast
            .builder(NodeKind::FileScope, sp)
            .add_multi_children(&[struct_def, impl_def, plus_fn, mod_const, main_fn])
            .build();
        ast.source = Some("".into());

        let sm = rustc_span::source_map::SourceMap::new(
            rustc_span::source_map::FilePathMapping::empty(),
        );
        let diag = diagnostic::DiagnosticContext::new(&sm);
        let resolved = resolution::resolve(ast, &diag);
        assert!(!diag.has_errors(), "{:?}", diag.diagnostics());
        let nir = try_lower(&resolved).expect("NIR lowering should succeed");

        // Verify module globals include G1 and G2
        assert_eq!(nir.globals.len(), 2);
        // Verify synthesized `<bound-method>` closure function exists
        assert!(
            nir.functions
                .iter()
                .any(|f| f.is_closure && str_interner::get(f.name) == "<bound-method>"),
            "expected synthesized <bound-method> closure"
        );
        // Verify main function contains LIST_APPEND and LIST_LEN for SpreadArg unpacking
        let main_nir = nir
            .functions
            .iter()
            .find(|f| str_interner::get(f.name) == "main")
            .expect("main NirFunction");
        let has_list_append = main_nir.blocks.iter().any(|b| {
            b.stmts.iter().any(|s| {
                matches!(
                    s,
                    NirStmt::Assign(_, NirExpr::CallBuiltin(id, _)) if *id == runtime::ids::LIST_APPEND
                )
            })
        });
        let has_list_len = main_nir.blocks.iter().any(|b| {
            b.stmts.iter().any(|s| {
                matches!(
                    s,
                    NirStmt::Assign(_, NirExpr::CallBuiltin(id, _)) if *id == runtime::ids::LIST_LEN
                )
            })
        });
        assert!(
            has_list_append,
            "expected LIST_APPEND in spread call lowering"
        );
        assert!(
            has_list_len,
            "expected LIST_LEN arity check in spread call lowering"
        );
    }
}
