//! Concrete default signatures must be checked against the values they return.

use crate::builder::FunctionBuilder;
use crate::{NirExpr, NirStmt, NirValue, Terminator};

pub(crate) fn check_returns(builder: &mut FunctionBuilder) {
    let scope = builder
        .entry_scope
        .expect("default specialization has its declaration scope");
    for index in 0..builder.blocks.len() {
        let Terminator::Return(value) = builder.blocks[index].terminator else {
            continue;
        };
        let local = builder.alloc_local();
        builder.blocks[index].stmts.push(NirStmt::Assign(
            local,
            NirExpr::ScopedCall {
                scope,
                call: Box::new(NirExpr::TypeAssert(value, builder.return_type)),
            },
        ));
        builder.blocks[index].terminator = Terminator::Return(NirValue::Local(local));
    }
}
