//! Compound assignment reads one checked place, applies the operator and
//! writes the same place back. A field receiver or an index receiver and key
//! are evaluated exactly once, before the right operand.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::{lower_expr, store_variable};
use crate::{BinOp, BlockId, NirExpr, NirStmt, NirValue};

enum Place {
    Variable,
    Field {
        receiver: NirValue,
        index: u32,
    },
    Index {
        callee: NodeIndex,
        receiver: NirValue,
        key: NirValue,
    },
}

fn operator(kind: NodeKind) -> BinOp {
    match kind {
        NodeKind::AddAssign => BinOp::Add,
        NodeKind::SubAssign => BinOp::Sub,
        NodeKind::MulAssign => BinOp::Mul,
        NodeKind::DivAssign => BinOp::Div,
        NodeKind::ModAssign => BinOp::Mod,
        _ => unreachable!("only compound assignment nodes are lowered here"),
    }
}

fn assign(builder: &mut FunctionBuilder, block: BlockId, expression: NirExpr) -> NirValue {
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, expression));
    NirValue::Local(local)
}

pub(crate) fn lower(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let ast = &resolved.ast;
    let op = operator(ast.node(node).kind);
    let children = ast.fixed_children(node);
    let target = children[0];
    let (place, current) = read(resolved, target, builder, block);
    let current = snapshot(current, builder, *block);
    let operand = lower_expr(resolved, children[1], builder, block);
    let mut value = assign(builder, *block, NirExpr::BinOp(op, current, operand));
    if let Some(ty) = builder.node_type(resolved, target)
        && resolved.type_pool.as_intrinsic(ty) != Some(type_pool::Intrinsic::Any)
    {
        value = assign(builder, *block, NirExpr::TypeCast(value, ty));
    }
    match place {
        Place::Variable => store_variable(resolved, target, value, builder, *block),
        Place::Field { receiver, index } => builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::StoreField(receiver, index, value)),
        Place::Index {
            callee,
            receiver,
            key,
        } => {
            let stored = crate::collections::store_index(
                resolved,
                callee,
                receiver,
                &[key],
                value,
                builder,
                *block,
            );
            assert!(stored, "resolution admits only List and Map index targets");
        }
    }
    NirValue::Unit
}

fn read(
    resolved: &ResolvedAst,
    target: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> (Place, NirValue) {
    let ast = &resolved.ast;
    match ast.node(target).kind {
        NodeKind::Projection => {
            if let Some(index) = builder.field_index(resolved, target) {
                let receiver = lower_expr(resolved, ast.fixed_children(target)[0], builder, block);
                let receiver = snapshot(receiver, builder, *block);
                let current = assign(builder, *block, NirExpr::FieldAccess(receiver, index));
                return (Place::Field { receiver, index }, current);
            }
        }
        NodeKind::Call => {
            let callee = ast.fixed_children(target)[0];
            let receiver = lower_expr(resolved, callee, builder, block);
            let receiver = snapshot(receiver, builder, *block);
            let key_node = resolution::argument_value_node(ast, ast.multi_children(target)[0]);
            let key = lower_expr(resolved, key_node, builder, block);
            let key = snapshot(key, builder, *block);
            let expression =
                crate::collections::index_access(resolved, callee, receiver, &[key], builder)
                    .expect("resolution admits only List and Map index targets");
            let current = assign(builder, *block, expression);
            return (
                Place::Index {
                    callee,
                    receiver,
                    key,
                },
                current,
            );
        }
        _ => {}
    }
    (
        Place::Variable,
        lower_expr(resolved, target, builder, block),
    )
}
