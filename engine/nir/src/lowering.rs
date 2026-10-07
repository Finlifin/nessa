//! NirLowering — orchestrates the lowering of a resolved AST to NIR.
//!
//! This module owns the `NirLowering` struct which walks the AST top-down,
//! discovers function definitions, and delegates body lowering to
//! [`crate::stmt`] and [`crate::expr`].
//!
//! Uses a two-pass approach:
//!   1. Collect all function definitions and assign FuncIds
//!   2. Lower function bodies with the complete SymbolId→FuncId map

use std::collections::HashMap;

use ast::{NodeIndex, NodeKind};
use nsbc::FuncId;
use resolution::{ResolvedAst, SymbolId};
use type_pool::{TypeIndex, TypeKind};

use crate::builder::FunctionBuilder;
use crate::initialization::InitializationPlan;
use crate::stmt::lower_body_into;
use crate::{NirExpr, NirModule, NirParam, NirStmt, NirValue, Terminator};

// ---------------------------------------------------------------------------
// NirLowering
// ---------------------------------------------------------------------------

/// Lowers a resolved AST into NIR basic-block form.
pub(crate) struct NirLowering {
    functions: Vec<crate::NirFunction>,
    next_func_id: u32,
    /// Maps function SymbolId → FuncId (populated in pass 1, used in pass 2).
    func_map: HashMap<SymbolId, FuncId>,
    /// Function definitions to lower in pass 2: (node_idx, func_id).
    pending: Vec<(NodeIndex, FuncId)>,
    derived_functions: HashMap<(TypeIndex, TypeIndex, str_interner::StrId), FuncId>,
    global_map: HashMap<SymbolId, nsbc::GlobalId>,
    default_function_maps: HashMap<SymbolId, HashMap<SymbolId, FuncId>>,
}

impl NirLowering {
    pub fn new() -> Self {
        Self {
            functions: Vec::new(),
            next_func_id: 0,
            func_map: HashMap::new(),
            pending: Vec::new(),
            derived_functions: HashMap::new(),
            global_map: HashMap::new(),
            default_function_maps: HashMap::new(),
        }
    }

    /// Pass 2: lower function bodies with the complete func_map.
    pub fn lower(
        mut self,
        resolved: &ResolvedAst,
        initialization: InitializationPlan,
        startup: crate::StartupMode,
    ) -> NirModule {
        self.global_map = initialization.slots;
        // Pass 1: collect all function definitions, assign FuncIds.
        self.collect_functions(resolved, resolved.ast.root);

        for plan in &resolved.default_methods {
            let function = FuncId(self.next_func_id);
            self.next_func_id += 1;
            self.func_map.insert(plan.function, function);
            let mut functions = HashMap::new();
            let mut nodes: Vec<_> = plan.self_function_types.keys().copied().collect();
            nodes.sort_by_key(|node| node.0);
            for node in nodes {
                if resolved.ast.node(node).kind == NodeKind::FunctionDef
                    && node != resolved.symbols[plan.declaration.0 as usize].def_node
                    && let Some(&symbol) = resolved
                        .node_symbols
                        .get(&resolved.ast.fixed_children(node)[0])
                {
                    functions.insert(symbol, FuncId(self.next_func_id));
                    self.next_func_id += 1;
                }
            }
            self.default_function_maps.insert(plan.function, functions);
        }

        for plan in &resolved.derived_comparisons {
            let function = FuncId(self.next_func_id);
            self.next_func_id += 1;
            self.derived_functions.insert(
                (plan.implementor, plan.trait_type, plan.method_name),
                function,
            );
        }
        for plan in &resolved.display_derivations {
            let function = FuncId(self.next_func_id);
            self.next_func_id += 1;
            self.derived_functions.insert(
                (plan.implementor, plan.trait_type, plan.method_name),
                function,
            );
        }

        // Pass 2: lower function bodies with the complete func_map.
        let pending = std::mem::take(&mut self.pending);
        for (node_idx, func_id) in pending {
            self.lower_function_def(resolved, node_idx, func_id, None);
        }
        for plan in &resolved.default_methods {
            self.lower_function_def(
                resolved,
                resolved.symbols[plan.declaration.0 as usize].def_node,
                self.func_map[&plan.function],
                Some(plan),
            );
            let mut nodes: Vec<_> = plan.self_function_types.keys().copied().collect();
            nodes.sort_by_key(|node| node.0);
            for node in nodes {
                if resolved.ast.node(node).kind == NodeKind::FunctionDef
                    && node != resolved.symbols[plan.declaration.0 as usize].def_node
                {
                    let symbol = resolved.node_symbols[&resolved.ast.fixed_children(node)[0]];
                    let function = self.default_function_maps[&plan.function][&symbol];
                    self.lower_function_def(resolved, node, function, Some(plan));
                }
            }
        }

        let main = match startup {
            crate::StartupMode::Main | crate::StartupMode::RequiredMain => {
                crate::root_main(resolved)
                    .and_then(|symbol| self.func_map.get(&symbol))
                    .copied()
            }
            crate::StartupMode::InitializationOnly => None,
        };
        let entry = self.lower_startup(resolved, initialization.ordered, main);
        let mut derived_methods = Vec::new();
        for plan in &resolved.derived_comparisons {
            let func_id =
                self.derived_functions[&(plan.implementor, plan.trait_type, plan.method_name)];
            self.functions.push(crate::derivation::lower(
                resolved,
                plan,
                func_id,
                &self.func_map,
                &self.derived_functions,
            ));
            derived_methods.push(crate::DerivedMethod {
                implementor: plan.implementor,
                trait_type: plan.trait_type,
                method_name: plan.method_name,
                func_id,
            });
        }
        for plan in &resolved.display_derivations {
            let func_id =
                self.derived_functions[&(plan.implementor, plan.trait_type, plan.method_name)];
            self.functions.push(crate::display_derivation::lower(
                resolved,
                plan,
                func_id,
                &self.func_map,
                &self.derived_functions,
            ));
            derived_methods.push(crate::DerivedMethod {
                implementor: plan.implementor,
                trait_type: plan.trait_type,
                method_name: plan.method_name,
                func_id,
            });
        }
        self.functions.sort_by_key(|function| function.func_id.0);
        NirModule {
            derived_methods,
            function_symbols: self.func_map,
            functions: self.functions,
            globals: initialization.globals,
            entry,
        }
    }

    fn lower_startup(
        &mut self,
        resolved: &ResolvedAst,
        initializers: Vec<crate::initialization::ScopeInitialization>,
        main: Option<FuncId>,
    ) -> Option<FuncId> {
        if initializers.is_empty() {
            return main;
        }
        let mut calls = Vec::new();
        for initializer in initializers {
            let function = FuncId(self.next_func_id);
            self.next_func_id += 1;
            let mut builder = FunctionBuilder::new(
                function,
                str_interner::intern(&format!("__module_init_{}", initializer.node.0)),
            );
            builder.func_map = self.func_map.clone();
            builder.derived_functions = self.derived_functions.clone();
            builder.global_map = self.global_map.clone();
            builder.next_func_id = self.next_func_id;
            builder.return_type = type_pool::Intrinsic::Unit.type_index();
            let mut block = builder.new_block();
            builder.entry_block = block;
            for statement in initializer.statements {
                crate::stmt::lower_stmt(resolved, statement, &mut builder, &mut block, false);
            }
            if let Some(hook) = initializer.hook {
                let result = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    result,
                    NirExpr::Call(self.func_map[&hook], Vec::new()),
                ));
            }
            builder.blocks[block.0 as usize].terminator = Terminator::Return(NirValue::Unit);
            self.next_func_id = builder.next_func_id;
            self.functions.append(&mut builder.lambda_functions);
            self.functions.push(builder.build());
            calls.push(function);
        }
        let entry = FuncId(self.next_func_id);
        self.next_func_id += 1;
        let mut builder = FunctionBuilder::new(entry, str_interner::intern("__startup"));
        let block = builder.new_block();
        builder.entry_block = block;
        for function in calls {
            let result = builder.alloc_local();
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(result, NirExpr::Call(function, Vec::new())));
        }
        let value = if let Some(main) = main {
            let result = builder.alloc_local();
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(result, NirExpr::Call(main, Vec::new())));
            NirValue::Local(result)
        } else {
            NirValue::Unit
        };
        builder.blocks[block.0 as usize].terminator = Terminator::Return(value);
        self.functions.push(builder.build());
        Some(entry)
    }

    /// Pass 1: walk the AST collecting function definitions.
    fn collect_functions(&mut self, resolved: &ResolvedAst, node_idx: NodeIndex) {
        if node_idx.is_null() {
            return;
        }
        let ast = &resolved.ast;
        let node = ast.node(node_idx);
        match node.kind {
            // Default bodies, including their named functions, are instantiated
            // through per-implementation plans rather than shared source IDs.
            NodeKind::TraitDeriveFn => {}
            NodeKind::FileScope => {
                for &child in ast.multi_children(node_idx) {
                    self.collect_functions(resolved, child);
                }
            }
            NodeKind::FunctionDef => {
                let children = ast.fixed_children(node_idx);
                let name_node = children[0];
                let func_id = FuncId(self.next_func_id);
                self.next_func_id += 1;

                // If the function name has a resolved symbol, record the mapping.
                if !name_node.is_null()
                    && let Some(&sym_id) = resolved.node_symbols.get(&name_node)
                {
                    self.func_map.insert(sym_id, func_id);
                }

                self.pending.push((node_idx, func_id));
                // Local associated definitions have independent source functions
                // even though their visibility is restricted to the enclosing body.
                for &child in ast
                    .fixed_children(node_idx)
                    .iter()
                    .chain(ast.multi_children(node_idx))
                {
                    self.collect_functions(resolved, child);
                }
            }
            _ => {
                // Recurse into children for nested definitions.
                for &child in ast.fixed_children(node_idx) {
                    self.collect_functions(resolved, child);
                }
                for &child in ast.multi_children(node_idx) {
                    self.collect_functions(resolved, child);
                }
            }
        }
    }

    /// Pass 2: lower a single function definition.
    ///
    /// FunctionDef children: [0] name  [1] return_type  [2] body  [3] capability
    /// multi = params
    fn lower_function_def(
        &mut self,
        resolved: &ResolvedAst,
        node_idx: NodeIndex,
        func_id: FuncId,
        default: Option<&resolution::DefaultMethodPlan>,
    ) {
        let ast = &resolved.ast;
        let children = ast.fixed_children(node_idx);
        let name_str = ast.node(children[0]).str_id;

        let mut builder = FunctionBuilder::new(func_id, name_str);
        builder.entry_scope = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == node_idx)
            .map(|scope| scope.id.0);
        // Give the builder access to the full function map for call resolution.
        builder.func_map = self.func_map.clone();
        builder.derived_functions = self.derived_functions.clone();
        builder.global_map = self.global_map.clone();
        // Set the next_func_id so lambdas get unique ids.
        builder.next_func_id = self.next_func_id;

        if let Some(symbol) = resolved.node_symbols.get(&children[0]) {
            builder.function_type = builder.symbol_type(resolved, *symbol);
            if let Some(signature) = resolved.type_pool.canonical_type(builder.function_type)
                && let TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind
            {
                builder.return_type = ret;
            }
        }

        if let Some(plan) = default {
            builder
                .func_map
                .extend(self.default_function_maps[&plan.function].clone());
            builder.function_type = plan
                .self_function_types
                .get(&node_idx)
                .copied()
                .unwrap_or(plan.signature);
            builder.body_facts = plan.body_facts.clone();
            builder.specialized_node_types = plan.specialized_node_types.clone();
            builder.specialized_symbol_types = plan.specialized_symbol_types.clone();
            builder.specialized_coercion_targets = plan.specialized_coercion_targets.clone();
            builder.self_type_values = plan.self_type_values.clone();
            builder.self_function_types = plan.self_function_types.clone();
            builder.self_function_parameters = plan.self_function_parameters.clone();
            builder.default_trait_view = Some(plan.trait_type);
            if let TypeKind::Function { ret, .. } =
                resolved.type_pool.get(builder.function_type).kind
            {
                builder.return_type = ret;
            }
        }

        // Add parameters.
        let params = ast.multi_children(node_idx);
        let mut tuple_parameters = Vec::new();
        for &param in params {
            let param_node = ast.node(param);
            match param_node.kind {
                NodeKind::ParamTyped => {
                    let param_children = ast.fixed_children(param);
                    let pat_node_idx = param_children[0];
                    let pat_node = ast.node(pat_node_idx);
                    let local = if let Some(&sym_id) = resolved.node_symbols.get(&pat_node_idx) {
                        builder.local_for_symbol(sym_id)
                    } else {
                        builder.alloc_local()
                    };
                    builder.params.push(NirParam {
                        local,
                        name: pat_node.str_id,
                        type_index: resolved
                            .node_symbols
                            .get(&pat_node_idx)
                            .map(|symbol| builder.symbol_type(resolved, *symbol))
                            .or_else(|| builder.node_type(resolved, pat_node_idx))
                            .unwrap_or(type_pool::TypeIndex::INVALID),
                        role: crate::NirParamRole::User,
                    });
                    if pat_node.kind == NodeKind::PatternTuple {
                        tuple_parameters.push((pat_node_idx, local));
                    }
                }
                NodeKind::ParamSelf => {
                    let symbol = resolved.node_symbols.get(&param).copied();
                    let local = symbol
                        .map(|symbol| builder.local_for_symbol(symbol))
                        .unwrap_or_else(|| builder.alloc_local());
                    builder.params.push(NirParam {
                        local,
                        name: symbol
                            .map(|symbol| resolved.symbols[symbol.0 as usize].name)
                            .unwrap_or(param_node.str_id),
                        type_index: symbol
                            .map(|symbol| builder.symbol_type(resolved, symbol))
                            .unwrap_or(TypeIndex::INVALID),
                        role: crate::NirParamRole::User,
                    });
                }
                NodeKind::ParamOptional => {
                    // .id : type = default
                    // children: [0] id  [1] type  [2] default
                    let param_children = ast.fixed_children(param);
                    let id_node_idx = param_children[0];
                    let id_node = ast.node(id_node_idx);
                    let local = if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                        builder.local_for_symbol(sym_id)
                    } else {
                        builder.alloc_local()
                    };
                    builder.params.push(NirParam {
                        local,
                        name: id_node.str_id,
                        type_index: resolved
                            .node_symbols
                            .get(&id_node_idx)
                            .map(|symbol| builder.symbol_type(resolved, *symbol))
                            .unwrap_or(TypeIndex::INVALID),
                        role: crate::NirParamRole::User,
                    });
                }
                NodeKind::ParamVarargs => {
                    // ...id (: type)?
                    // children: [0] id  [1] type
                    let param_children = ast.fixed_children(param);
                    let id_node_idx = param_children[0];
                    let id_node = ast.node(id_node_idx);
                    let local = if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                        builder.local_for_symbol(sym_id)
                    } else {
                        builder.alloc_local()
                    };
                    builder.params.push(NirParam {
                        local,
                        name: id_node.str_id,
                        type_index: resolved
                            .node_symbols
                            .get(&id_node_idx)
                            .map(|symbol| builder.symbol_type(resolved, *symbol))
                            .unwrap_or(TypeIndex::INVALID),
                        role: crate::NirParamRole::User,
                    });
                }
                NodeKind::ParamLambda => {
                    // lambda pattern (: type)?
                    // children: [0] pattern  [1] type
                    let param_children = ast.fixed_children(param);
                    let pat_node_idx = param_children[0];
                    let pat_node = ast.node(pat_node_idx);
                    let local = if let Some(&sym_id) = resolved.node_symbols.get(&pat_node_idx) {
                        builder.local_for_symbol(sym_id)
                    } else {
                        builder.alloc_local()
                    };
                    builder.params.push(NirParam {
                        local,
                        name: pat_node.str_id,
                        type_index: TypeIndex::INVALID,
                        role: crate::NirParamRole::User,
                    });
                }
                _ => {}
            }
        }

        if let Some(plan) = default {
            let TypeKind::Function { params, .. } =
                &resolved.type_pool.get(builder.function_type).kind
            else {
                unreachable!("default plan carries a checked function signature")
            };
            for (parameter, &ty) in builder.params.iter_mut().zip(params) {
                parameter.type_index = ty;
            }
            let positions = plan
                .self_function_parameters
                .get(&node_idx)
                .map(Vec::as_slice)
                .unwrap_or(&plan.self_parameters);
            crate::trait_parameters::prepare_self_parameters(
                resolved,
                plan.trait_type,
                positions,
                &mut builder,
            );
        } else {
            crate::trait_parameters::prepare_parameters(resolved, &mut builder);
        }
        // Create entry block.
        let mut entry = builder.new_block();
        builder.entry_block = entry;
        for (pattern, local) in tuple_parameters {
            crate::tuples::bind_pattern(
                resolved,
                pattern,
                NirValue::Local(local),
                &mut builder,
                entry,
            );
        }

        // Lower the body into the entry block.
        // FunctionDef children[2] = body (NOT children[3], which is capability).
        if children.len() > 2 && !children[2].is_null() {
            lower_body_into(resolved, children[2], &mut builder, &mut entry);
        } else {
            // Empty function body: return unit.
            builder.blocks[entry.0 as usize].terminator = Terminator::Return(NirValue::Unit);
        }

        let source_return = resolved
            .node_symbols
            .get(&children[0])
            .and_then(|symbol| {
                resolved
                    .type_pool
                    .canonical_type(builder.symbol_type(resolved, *symbol))
            })
            .and_then(|signature| match resolved.type_pool.get(signature).kind {
                TypeKind::Function { ret, .. } => Some(ret),
                _ => None,
            });
        if default.is_some()
            && (source_return != Some(builder.return_type)
                || !builder.specialized_node_types.is_empty())
        {
            crate::self_specialization::check_returns(&mut builder);
        }

        // Reclaim the next_func_id counter (lambdas may have incremented it).
        self.next_func_id = builder.next_func_id;
        // Collect any lambda functions generated during lowering.
        let lambda_fns = std::mem::take(&mut builder.lambda_functions);

        self.functions.push(builder.build());

        // Append lambda functions after the parent function.
        self.functions.extend(lambda_fns);
    }
}

impl Default for NirLowering {
    fn default() -> Self {
        Self::new()
    }
}
