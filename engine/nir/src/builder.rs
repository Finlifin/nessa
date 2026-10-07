//! FunctionBuilder — helper for incrementally constructing a [`NirFunction`].

use std::collections::{HashMap, HashSet};

use nsbc::{FuncId, GlobalId};
use resolution::SymbolId;
use str_interner::StrId;
use type_pool::TypeIndex;

use crate::{BasicBlock, BlockId, NirFunction, NirLocal, NirParam, Terminator};

// ---------------------------------------------------------------------------
// FunctionBuilder
// ---------------------------------------------------------------------------

pub(crate) struct LoopTargets {
    pub label: Option<StrId>,
    pub break_to: BlockId,
    pub continue_to: BlockId,
}

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
    pub entry_scope: Option<u32>,
    pub func_id: FuncId,
    pub name: StrId,
    pub function_type: TypeIndex,
    pub display_owner: Option<TypeIndex>,
    pub params: Vec<NirParam>,
    pub value_proofs: HashMap<crate::NirLocal, (crate::NirLocal, TypeIndex)>,
    /// Source bindings without a bare trait type erase local proof sidecars.
    pub proof_erased_locals: HashSet<crate::NirLocal>,
    /// Checked concrete types for symbolic associated occurrences in this adapter.
    pub body_facts: Option<resolution::DefaultBodyFacts>,
    pub specialized_node_types: HashMap<ast::NodeIndex, TypeIndex>,
    pub specialized_symbol_types: HashMap<SymbolId, TypeIndex>,
    pub specialized_coercion_targets: HashMap<ast::NodeIndex, TypeIndex>,
    /// Source Self and associated type expressions specialize without guessing by index.
    pub self_type_values: HashMap<ast::NodeIndex, TypeIndex>,
    pub self_function_types: HashMap<ast::NodeIndex, TypeIndex>,
    pub self_function_parameters: HashMap<ast::NodeIndex, Vec<usize>>,
    pub default_trait_view: Option<TypeIndex>,
    pub blocks: Vec<BasicBlock>,
    pub entry_block: BlockId,
    pub return_type: TypeIndex,
    next_local: u32,
    /// Maps resolved SymbolId to function-local NirLocal.
    pub symbol_to_local: HashMap<SymbolId, NirLocal>,
    /// Maps function SymbolId → FuncId for call resolution.
    pub func_map: HashMap<SymbolId, FuncId>,
    /// Shared storage identities, including imported aliases.
    pub derived_functions: HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
    pub global_map: HashMap<SymbolId, GlobalId>,
    /// Native adapters are keyed by both native ID and the declared signature.
    pub native_adapters: HashMap<(runtime::BuiltinFnId, TypeIndex), FuncId>,
    /// Lambda functions generated during expression lowering.
    pub lambda_functions: Vec<NirFunction>,
    /// Shared counter for allocating FuncIds (lambdas need new unique ids).
    pub next_func_id: u32,
    /// Whether this function is a closure.
    pub is_closure: bool,
    pub loop_targets: Vec<LoopTargets>,
    /// Source call proven to be the only use of a fresh handler continuation.
    pub consume_continuation_at: Option<ast::NodeIndex>,
}

impl FunctionBuilder {
    pub fn node_type(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<TypeIndex> {
        if self
            .body_facts
            .as_ref()
            .is_some_and(|facts| facts.nodes.contains(&node))
        {
            self.specialized_node_types.get(&node).copied()
        } else {
            self.specialized_node_types
                .get(&node)
                .or_else(|| resolved.node_types.get(&node))
                .copied()
        }
    }

    pub fn symbol_type(&self, resolved: &resolution::ResolvedAst, symbol: SymbolId) -> TypeIndex {
        self.specialized_symbol_types
            .get(&symbol)
            .copied()
            .unwrap_or(resolved.symbols[symbol.0 as usize].type_index)
    }

    pub fn error_construction(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::ErrorConstructionPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.error_constructions)
            .unwrap_or(&resolved.error_constructions)
            .get(&node)
            .cloned()
    }
    pub fn error_propagation(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::ErrorPropagationPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.error_propagations)
            .unwrap_or(&resolved.error_propagations)
            .get(&node)
            .cloned()
    }
    pub fn error_elimination(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::ErrorEliminationPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.error_eliminations)
            .unwrap_or(&resolved.error_eliminations)
            .get(&node)
            .cloned()
    }
    pub fn error_pattern(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::ErrorPatternPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.error_patterns)
            .unwrap_or(&resolved.error_patterns)
            .get(&node)
            .cloned()
    }
    pub fn error_conversions(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Vec<resolution::ErrorConversionPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.error_conversions)
            .unwrap_or(&resolved.error_conversions)
            .get(&node)
            .cloned()
            .unwrap_or_default()
    }
    pub fn coercion_target(&self, node: ast::NodeIndex, fallback: TypeIndex) -> TypeIndex {
        self.specialized_coercion_targets
            .get(&node)
            .copied()
            .unwrap_or(fallback)
    }

    pub fn node_symbol(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::SymbolId> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.node_symbols)
            .unwrap_or(&resolved.node_symbols)
            .get(&node)
            .copied()
    }
    pub fn field_index(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<u32> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.node_field_indices)
            .unwrap_or(&resolved.node_field_indices)
            .get(&node)
            .copied()
    }
    pub fn type_value(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<TypeIndex> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.node_type_values)
            .unwrap_or(&resolved.node_type_values)
            .get(&node)
            .copied()
    }
    pub fn for_loop<'a>(
        &'a self,
        resolved: &'a resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<&'a resolution::ForLoopPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.for_loops)
            .unwrap_or(&resolved.for_loops)
            .get(&node)
    }
    pub fn coercion(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::Coercion> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.node_coercions)
            .unwrap_or(&resolved.node_coercions)
            .get(&node)
            .copied()
    }
    pub fn enum_variant(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::EnumVariantRef> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.enum_variants)
            .unwrap_or(&resolved.enum_variants)
            .get(&node)
            .copied()
    }
    pub fn instance_method(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::SymbolId> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.instance_methods)
            .unwrap_or(&resolved.instance_methods)
            .get(&node)
            .copied()
    }
    pub fn application_call(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::SymbolId> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.application_calls)
            .unwrap_or(&resolved.application_calls)
            .get(&node)
            .copied()
    }
    pub fn update_call(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::SymbolId> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.update_calls)
            .unwrap_or(&resolved.update_calls)
            .get(&node)
            .copied()
    }
    pub fn concat_call(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::SymbolId> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.concat_calls)
            .unwrap_or(&resolved.concat_calls)
            .get(&node)
            .copied()
    }
    pub fn call_plan(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::CallArgumentPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.call_arguments)
            .unwrap_or(&resolved.call_arguments)
            .get(&node)
            .cloned()
    }
    pub fn struct_plan(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::StructConstructionPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.struct_constructions)
            .unwrap_or(&resolved.struct_constructions)
            .get(&node)
            .cloned()
    }
    pub fn enum_plan(
        &self,
        resolved: &resolution::ResolvedAst,
        node: ast::NodeIndex,
    ) -> Option<resolution::EnumConstructionPlan> {
        self.body_facts
            .as_ref()
            .filter(|facts| facts.nodes.contains(&node))
            .map(|facts| &facts.enum_constructions)
            .unwrap_or(&resolved.enum_constructions)
            .get(&node)
            .cloned()
    }
    pub fn new(func_id: FuncId, name: StrId) -> Self {
        Self {
            entry_scope: None,
            func_id,
            name,
            function_type: TypeIndex::INVALID,
            display_owner: None,
            params: Vec::new(),
            value_proofs: HashMap::new(),
            proof_erased_locals: HashSet::new(),
            body_facts: None,
            specialized_node_types: HashMap::new(),
            specialized_symbol_types: HashMap::new(),
            specialized_coercion_targets: HashMap::new(),
            self_type_values: HashMap::new(),
            self_function_types: HashMap::new(),
            self_function_parameters: HashMap::new(),
            default_trait_view: None,
            blocks: Vec::new(),
            entry_block: BlockId(0),
            return_type: TypeIndex::INVALID,
            next_local: 0,
            symbol_to_local: HashMap::new(),
            func_map: HashMap::new(),
            derived_functions: HashMap::new(),
            global_map: HashMap::new(),
            native_adapters: HashMap::new(),
            lambda_functions: Vec::new(),
            next_func_id: 0,
            is_closure: false,
            loop_targets: Vec::new(),
            consume_continuation_at: None,
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
            entry_scope: self.entry_scope,
            func_id: self.func_id,
            name: self.name,
            function_type: self.function_type,
            display_owner: self.display_owner,
            params: self.params,
            return_type: self.return_type,
            blocks: self.blocks,
            entry_block: self.entry_block,
            local_count: self.next_local,
            is_closure: self.is_closure,
        }
    }
}
