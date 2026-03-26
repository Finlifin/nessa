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
use type_pool::TypeIndex;

use crate::builder::FunctionBuilder;
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
}

impl NirLowering {
    pub fn new() -> Self {
        Self {
            functions: Vec::new(),
            next_func_id: 0,
            func_map: HashMap::new(),
            pending: Vec::new(),
        }
    }

    /// Pass 2: lower function bodies with the complete func_map.
    pub fn lower(mut self, resolved: &ResolvedAst) -> NirModule {
        // Pass 1: collect all function definitions, assign FuncIds.
        self.collect_functions(resolved, resolved.ast.root);

        // Pass 2: lower function bodies with the complete func_map.
        let pending = std::mem::take(&mut self.pending);
        for (node_idx, func_id) in pending {
            self.lower_function_def(resolved, node_idx, func_id);
        }

        NirModule {
            functions: self.functions,
        }
    }

    /// Pass 1: walk the AST collecting function definitions.
    fn collect_functions(&mut self, resolved: &ResolvedAst, node_idx: NodeIndex) {
        if node_idx.is_null() {
            return;
        }
        let ast = &resolved.ast;
        let node = ast.node(node_idx);
        match node.kind {
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
                if !name_node.is_null() {
                    if let Some(&sym_id) = resolved.node_symbols.get(&name_node) {
                        self.func_map.insert(sym_id, func_id);
                    }
                }

                self.pending.push((node_idx, func_id));
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
    fn lower_function_def(&mut self, resolved: &ResolvedAst, node_idx: NodeIndex, func_id: FuncId) {
        let ast = &resolved.ast;
        let children = ast.fixed_children(node_idx);
        let name_str = ast.node(children[0]).str_id;

        let mut builder = FunctionBuilder::new(func_id, name_str);
        // Give the builder access to the full function map for call resolution.
        builder.func_map = self.func_map.clone();
        // Set the next_func_id so lambdas get unique ids.
        builder.next_func_id = self.next_func_id;

        // Resolve return type if present.
        if !children[1].is_null() {
            // TODO: look up the actual TypeIndex from resolved type info.
            builder.return_type = TypeIndex::INVALID;
        }

        // Add parameters.
        let params = ast.multi_children(node_idx);
        let mut optional_defaults: Vec<(crate::NirLocal, NodeIndex)> = Vec::new();
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
                        type_index: TypeIndex::INVALID,
                        is_evidence: false,
                    });
                }
                NodeKind::ParamSelf => {
                    let local = builder.alloc_local();
                    builder.params.push(NirParam {
                        local,
                        name: param_node.str_id,
                        type_index: TypeIndex::INVALID,
                        is_evidence: false,
                    });
                }
                NodeKind::ParamOptional => {
                    // .id : type = default
                    // children: [0] id  [1] type  [2] default
                    let param_children = ast.fixed_children(param);
                    let id_node_idx = param_children[0];
                    let id_node = ast.node(id_node_idx);
                    let local =
                        if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                            builder.local_for_symbol(sym_id)
                        } else {
                            builder.alloc_local()
                        };
                    builder.params.push(NirParam {
                        local,
                        name: id_node.str_id,
                        type_index: TypeIndex::INVALID,
                        is_evidence: false,
                    });
                    // Store the default value AST node for later initialization.
                    if param_children.len() > 2 && !param_children[2].is_null() {
                        optional_defaults.push((local, param_children[2]));
                    }
                }
                NodeKind::ParamVarargs => {
                    // ...id (: type)?
                    // children: [0] id  [1] type
                    let param_children = ast.fixed_children(param);
                    let id_node_idx = param_children[0];
                    let id_node = ast.node(id_node_idx);
                    let local =
                        if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                            builder.local_for_symbol(sym_id)
                        } else {
                            builder.alloc_local()
                        };
                    builder.params.push(NirParam {
                        local,
                        name: id_node.str_id,
                        type_index: TypeIndex::INVALID,
                        is_evidence: false,
                    });
                }
                NodeKind::ParamLambda => {
                    // lambda pattern (: type)?
                    // children: [0] pattern  [1] type
                    let param_children = ast.fixed_children(param);
                    let pat_node_idx = param_children[0];
                    let pat_node = ast.node(pat_node_idx);
                    let local =
                        if let Some(&sym_id) = resolved.node_symbols.get(&pat_node_idx) {
                            builder.local_for_symbol(sym_id)
                        } else {
                            builder.alloc_local()
                        };
                    builder.params.push(NirParam {
                        local,
                        name: pat_node.str_id,
                        type_index: TypeIndex::INVALID,
                        is_evidence: false,
                    });
                }
                _ => {}
            }
        }

        // Create entry block.
        let mut entry = builder.new_block();
        builder.entry_block = entry;

        // Emit default value assignments for optional parameters.
        // At runtime, the caller may omit optional args, so we initialize
        // them with their default expressions at the top of the function.
        // TODO: check at runtime whether the argument was actually supplied
        //       and skip the default initialization if so.
        for (local, default_node) in optional_defaults {
            use crate::expr::lower_expr;
            let val = lower_expr(resolved, default_node, &mut builder, &mut entry);
            builder.blocks[entry.0 as usize]
                .stmts
                .push(NirStmt::Assign(local, NirExpr::Use(val)));
        }

        // Lower the body into the entry block.
        // FunctionDef children[2] = body (NOT children[3], which is capability).
        if children.len() > 2 && !children[2].is_null() {
            lower_body_into(resolved, children[2], &mut builder, &mut entry);
        } else {
            // Empty function body: return unit.
            builder.blocks[entry.0 as usize].terminator = Terminator::Return(NirValue::Unit);
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
