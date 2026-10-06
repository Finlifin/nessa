mod builder;
mod expr;
mod lowering;
mod stmt;

use nsbc::FuncId;
use runtime::BuiltinFnId;
use resolution::ResolvedAst;
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
    /// An f64 constant.
    ConstFloat(f64),
    /// A boolean constant.
    ConstBool(bool),
    /// A string constant (interned).
    ConstStr(StrId),
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
    And,
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
    /// A value (local or constant).
    Use(NirValue),
    /// Binary operation.
    BinOp(BinOp, NirValue, NirValue),
    /// Unary operation.
    UnaryOp(UnaryOp, NirValue),
    /// Function call.
    Call(FuncId, Vec<NirValue>),
    /// Builtin function call (native VM implementation via CallBuiltin).
    CallBuiltin(BuiltinFnId, Vec<NirValue>),
    /// Method call: receiver, method name, args.
    MethodCall(NirValue, StrId, Vec<NirValue>),
    /// Field access: object, field index.
    FieldAccess(NirValue, u32),
    /// Index access: object, index.
    IndexAccess(NirValue, NirValue),
    /// Allocate a new object of the given type with field values.
    NewObject(TypeIndex, Vec<NirValue>),
    /// Create a closure: function id, captured values.
    NewClosure(FuncId, Vec<NirValue>),
    /// Indirect call through a closure value: callee, args.
    CallIndirect(NirValue, Vec<NirValue>),
    /// Runtime type check.
    TypeCheck(NirValue, TypeIndex),
    /// Type cast.
    TypeCast(NirValue, TypeIndex),
}

// ---------------------------------------------------------------------------
// NirStmt — statements in a basic block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NirStmt {
    /// Assign an expression to a local.
    Assign(NirLocal, NirExpr),
    /// Drop a local (explicit release for move semantics).
    Drop(NirLocal),
    /// Effect call via evidence: evidence_local, operation_index, args, result_local.
    EffectCall {
        evidence: NirLocal,
        operation: u32,
        args: Vec<NirValue>,
        result: NirLocal,
    },
    /// No operation.
    Nop,
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

#[derive(Debug, Clone)]
pub struct NirParam {
    pub local: NirLocal,
    pub name: StrId,
    pub type_index: TypeIndex,
    /// Whether this is an implicit evidence parameter.
    pub is_evidence: bool,
}

// ---------------------------------------------------------------------------
// NirFunction — a single function in NIR form
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NirFunction {
    pub func_id: FuncId,
    pub name: StrId,
    pub params: Vec<NirParam>,
    pub return_type: TypeIndex,
    pub blocks: Vec<BasicBlock>,
    pub entry_block: BlockId,
    /// Number of locals allocated (including params).
    pub local_count: u32,
    /// Whether this function is a closure (lambda with captures).
    pub is_closure: bool,
}

// ---------------------------------------------------------------------------
// NirModule — the complete NIR for a compilation unit
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct NirModule {
    pub functions: Vec<NirFunction>,
}

// ---------------------------------------------------------------------------
// Convenience entry point
// ---------------------------------------------------------------------------

/// Lower a resolved AST to NIR.
pub fn lower(resolved: &ResolvedAst) -> NirModule {
    lowering::NirLowering::new().lower(resolved)
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
        let interner = &mut str_interner::Interner::new();
        let foo_str = interner.intern("foo");

        // Int literal
        let int_node = ast.builder(NodeKind::Int, rustc_span::DUMMY_SP).build();

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
}
