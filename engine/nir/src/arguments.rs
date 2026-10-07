//! Bind checked source arguments to the fixed runtime parameter layout.

use ast::NodeIndex;
use resolution::{CallArgumentValue, ResolvedAst};

use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue};

/// Freeze a computed local before later expressions can overwrite its storage.
pub(crate) fn snapshot(value: NirValue, builder: &mut FunctionBuilder, block: BlockId) -> NirValue {
    if matches!(value, NirValue::Local(_)) {
        let local = builder.alloc_local();
        crate::trait_parameters::copy_proof(local, value, builder, block);
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, NirExpr::Use(value)));
        NirValue::Local(local)
    } else {
        value
    }
}

pub(crate) fn lower_arguments(
    resolved: &ResolvedAst,
    call: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> Vec<NirValue> {
    let explicit: Vec<_> = resolved
        .ast
        .multi_children(call)
        .iter()
        .map(|&argument| {
            let value = lower_expr(
                resolved,
                resolution::argument_value_node(&resolved.ast, argument),
                builder,
                block,
            );
            snapshot(value, builder, *block)
        })
        .collect();
    bind_arguments(resolved, call, explicit, None, builder, block)
}

pub(crate) fn lower_method_arguments(
    resolved: &ResolvedAst,
    node: NodeIndex,
    receiver: NirValue,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> Vec<NirValue> {
    let assignment = resolved.ast.node(node).kind == ast::NodeKind::Assign;
    let call = if assignment {
        resolved.ast.fixed_children(node)[0]
    } else {
        node
    };
    let mut explicit: Vec<_> = resolved
        .ast
        .multi_children(call)
        .iter()
        .map(|&argument| {
            let value = lower_expr(
                resolved,
                resolution::argument_value_node(&resolved.ast, argument),
                builder,
                block,
            );
            snapshot(value, builder, *block)
        })
        .collect();
    if assignment {
        let value = lower_expr(
            resolved,
            resolved.ast.fixed_children(node)[1],
            builder,
            block,
        );
        explicit.push(snapshot(value, builder, *block));
    }
    bind_arguments(resolved, node, explicit, Some(receiver), builder, block)
}

fn bind_arguments(
    resolved: &ResolvedAst,
    call: NodeIndex,
    explicit: Vec<NirValue>,
    receiver: Option<NirValue>,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> Vec<NirValue> {
    let Some(plan) = builder.call_plan(resolved, call) else {
        return explicit;
    };
    let mut arguments = Vec::with_capacity(plan.parameters.len());
    let mut previous = Vec::new();
    if let Some(receiver) = receiver {
        let parameter = resolved.ast.multi_children(plan.declaration)[0];
        let symbol = builder
            .node_symbol(resolved, parameter)
            .expect("checked receiver parameter");
        let local = match receiver {
            NirValue::Local(local) => local,
            _ => {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::Use(receiver)));
                local
            }
        };
        previous.push((symbol, builder.symbol_to_local.insert(symbol, local)));
    }
    for binding in &plan.parameters {
        let value = match &binding.value {
            CallArgumentValue::Explicit { source_index } => explicit[*source_index],
            CallArgumentValue::Variadic { source_indices } => {
                let elements: Vec<_> = source_indices
                    .iter()
                    .map(|&index| explicit[index])
                    .collect();
                crate::lists::build_list(&elements, builder, *block)
            }
            CallArgumentValue::MapVariadic { source_properties } => {
                let map = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    map,
                    NirExpr::CallBuiltin(runtime::ids::MAP_INIT, Vec::new()),
                ));
                for &(key, index) in source_properties {
                    let result = builder.alloc_local();
                    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                        result,
                        NirExpr::CallBuiltin(
                            runtime::ids::MAP_SET,
                            vec![
                                NirValue::Local(map),
                                NirValue::ConstStr(key),
                                explicit[index],
                            ],
                        ),
                    ));
                }
                NirValue::Local(map)
            }
            CallArgumentValue::Default { expression, .. } => {
                let mut value = lower_expr(resolved, *expression, builder, block);
                // Freeze the default's implementation in its declaration scope,
                // before snapshots and parameter-order argument expansion.
                if let Some(view) = binding.symbols.first().and_then(|symbol| {
                    crate::trait_parameters::bare_trait(
                        resolved,
                        builder.symbol_type(resolved, *symbol),
                    )
                }) {
                    let local = match value {
                        NirValue::Local(local) => local,
                        _ => {
                            let local = builder.alloc_local();
                            builder.blocks[block.0 as usize]
                                .stmts
                                .push(NirStmt::Assign(local, NirExpr::Use(value)));
                            local
                        }
                    };
                    let proof = crate::trait_parameters::proof_for(
                        resolved,
                        *expression,
                        NirValue::Local(local),
                        view,
                        builder,
                        *block,
                    );
                    let checked = builder.alloc_local();
                    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                        checked,
                        NirExpr::TraitAssert(NirValue::Local(local), proof, view)
                            .in_source_scope(resolved, *expression),
                    ));
                    if let NirValue::Local(proof) = proof {
                        builder.value_proofs.insert(checked, (proof, view));
                    }
                    value = NirValue::Local(checked);
                }
                snapshot(value, builder, *block)
            }
        };
        // Parameter references in defaults resolve to the declaration's symbols.
        // Temporarily substitute the already bound values, including recursive
        // calls where those same symbols belong to the caller's own parameters.
        let local = match value {
            NirValue::Local(local) => local,
            _ => {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::Use(value)));
                local
            }
        };
        let pattern = resolved.ast.fixed_children(binding.parameter)[0];
        if resolved.ast.node(pattern).kind == ast::NodeKind::PatternTuple {
            for &symbol in &binding.symbols {
                let old = builder.symbol_to_local.remove(&symbol);
                previous.push((symbol, old));
            }
            crate::tuples::bind_pattern(resolved, pattern, NirValue::Local(local), builder, *block);
        } else {
            for &symbol in &binding.symbols {
                previous.push((symbol, builder.symbol_to_local.insert(symbol, local)));
            }
        }
        arguments.push(value);
    }
    for (symbol, old) in previous.into_iter().rev() {
        match old {
            Some(local) => {
                builder.symbol_to_local.insert(symbol, local);
            }
            None => {
                builder.symbol_to_local.remove(&symbol);
            }
        }
    }
    arguments
}

#[cfg(test)]
mod tests {
    use ast::{NodeIndex, NodeKind};
    use nsbc::FuncId;
    use resolution::{CallArgumentPlan, CallArgumentValue, CallParameterBinding};
    use rustc_span::DUMMY_SP;

    use super::{FunctionBuilder, NirExpr, NirStmt, NirValue, lower_arguments};

    #[test]
    fn large_interleaved_extended_entries_bind_only_two_rooted_containers() {
        let (mut resolved, _) = crate::literal::tests::resolved_integer("42", false, "i64");
        let mut entries = Vec::new();
        let mut list_indices = Vec::new();
        let mut properties = Vec::new();
        let key = str_interner::intern("answer");
        for index in 0..42 {
            let value = resolved
                .ast
                .builder(NodeKind::Int, DUMMY_SP)
                .set_str_id(str_interner::intern(&index.to_string()))
                .build();
            let entry = if index == 1 || index == 41 {
                let id = resolved
                    .ast
                    .builder(NodeKind::Id, DUMMY_SP)
                    .set_str_id(key)
                    .build();
                properties.push((key, index));
                resolved
                    .ast
                    .builder(NodeKind::Property, DUMMY_SP)
                    .add_child(id)
                    .add_child(value)
                    .build()
            } else {
                list_indices.push(index);
                value
            };
            entries.push(entry);
        }
        let parameters: Vec<_> = ["children", "properties"]
            .iter()
            .map(|name| {
                let id = resolved
                    .ast
                    .builder(NodeKind::Id, DUMMY_SP)
                    .set_str_id(str_interner::intern(name))
                    .build();
                resolved
                    .ast
                    .builder(NodeKind::ParamVarargs, DUMMY_SP)
                    .add_child(id)
                    .add_child(NodeIndex::NULL)
                    .build()
            })
            .collect();
        let call = resolved
            .ast
            .builder(NodeKind::ExtendedCall, DUMMY_SP)
            .add_child(NodeIndex::NULL)
            .add_multi_children(&entries)
            .build();
        resolved.call_arguments.insert(
            call,
            CallArgumentPlan {
                declaration: NodeIndex::NULL,
                parameters: vec![
                    CallParameterBinding {
                        parameter: parameters[0],
                        symbols: Vec::new(),
                        value: CallArgumentValue::Variadic {
                            source_indices: list_indices,
                        },
                    },
                    CallParameterBinding {
                        parameter: parameters[1],
                        symbols: Vec::new(),
                        value: CallArgumentValue::MapVariadic {
                            source_properties: properties,
                        },
                    },
                ],
            },
        );
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("main"));
        let mut block = builder.new_block();
        let arguments = lower_arguments(&resolved, call, &mut builder, &mut block);
        assert_eq!(arguments.len(), 2);
        assert!(
            arguments
                .iter()
                .all(|value| matches!(value, NirValue::Local(_)))
        );
        let statements = &builder.blocks[block.0 as usize].stmts;
        assert!(matches!(
            statements.first(),
            Some(NirStmt::Assign(_, NirExpr::NewList(40)))
        ));
        assert_eq!(
            statements
                .iter()
                .filter(|statement| matches!(statement, NirStmt::StoreIndex(..)))
                .count(),
            40
        );
        let sets: Vec<_> = statements
            .iter()
            .filter_map(|statement| match statement {
                NirStmt::Assign(_, NirExpr::CallBuiltin(id, arguments))
                    if *id == runtime::ids::MAP_SET =>
                {
                    Some(arguments)
                }
                _ => None,
            })
            .collect();
        assert_eq!(sets.len(), 2);
        assert_eq!(
            sets[0],
            &vec![arguments[1], NirValue::ConstStr(key), NirValue::ConstInt(1)]
        );
        assert_eq!(
            sets[1],
            &vec![
                arguments[1],
                NirValue::ConstStr(key),
                NirValue::ConstInt(41)
            ]
        );
    }
}
