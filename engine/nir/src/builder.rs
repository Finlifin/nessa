//! FunctionBuilder — helper for incrementally constructing a [`NirFunction`].

use std::collections::HashMap;

use nsbc::FuncId;
use resolution::SymbolId;
use str_interner::StrId;
use type_pool::TypeIndex;

use crate::{BasicBlock, BlockId, NirFunction, NirLocal, NirParam, Terminator};

// ---------------------------------------------------------------------------
// FunctionBuilder
// ---------------------------------------------------------------------------

/// Incremental builder for [`NirFunction`].
///
/// Usage:
/// ```ignore
/// let mut b = FunctionBuilder::new(func_id, name);
/// let entry = b.new_block();
/// b.entry_block = entry;
/// // ... emit stmts and terminators ...
/// let func = b.build();
/// ```
pub(crate) struct FunctionBuilder {
    pub func_id: FuncId,
    pub name: StrId,
    pub params: Vec<NirParam>,
    pub blocks: Vec<BasicBlock>,
    pub entry_block: BlockId,
    pub return_type: TypeIndex,
    next_local: u32,
    /// Maps resolved SymbolId to function-local NirLocal.
    pub symbol_to_local: HashMap<SymbolId, NirLocal>,
    /// Maps function SymbolId → FuncId for call resolution.
    pub func_map: HashMap<SymbolId, FuncId>,
    /// Lambda functions generated during expression lowering.
    pub lambda_functions: Vec<NirFunction>,
    /// Shared counter for allocating FuncIds (lambdas need new unique ids).
    pub next_func_id: u32,
    /// Whether this function is a closure.
    pub is_closure: bool,
}

impl FunctionBuilder {
    pub fn new(func_id: FuncId, name: StrId) -> Self {
        Self {
            func_id,
            name,
            params: Vec::new(),
            blocks: Vec::new(),
            entry_block: BlockId(0),
            return_type: TypeIndex::INVALID,
            next_local: 0,
            symbol_to_local: HashMap::new(),
            func_map: HashMap::new(),
            lambda_functions: Vec::new(),
            next_func_id: 0,
            is_closure: false,
        }
    }

    /// Allocate a fresh local variable / temporary.
    pub fn alloc_local(&mut self) -> NirLocal {
        let l = NirLocal(self.next_local);
        self.next_local += 1;
        l
    }

    /// Get or allocate a local for a resolved SymbolId.
    pub fn local_for_symbol(&mut self, sym: SymbolId) -> NirLocal {
        if let Some(&local) = self.symbol_to_local.get(&sym) {
            local
        } else {
            let local = self.alloc_local();
            self.symbol_to_local.insert(sym, local);
            local
        }
    }

    /// Create a new empty basic block and return its id.
    pub fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock {
            id,
            stmts: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    /// Allocate a new unique FuncId for a lambda function.
    pub fn alloc_func_id(&mut self) -> FuncId {
        let id = FuncId(self.next_func_id);
        self.next_func_id += 1;
        id
    }

    /// Consume the builder and produce a [`NirFunction`].
    pub fn build(self) -> NirFunction {
        NirFunction {
            func_id: self.func_id,
            name: self.name,
            params: self.params,
            return_type: self.return_type,
            blocks: self.blocks,
            entry_block: self.entry_block,
            local_count: self.next_local,
            is_closure: self.is_closure,
        }
    }
}
