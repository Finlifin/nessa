//! Native-backed derivations publish ordinary typed functions to trait tables.

use nsbc::FuncId;
use resolution::{DerivedDisplayPlan, ResolvedAst};
use type_pool::Intrinsic;

use crate::builder::FunctionBuilder;
use crate::{NirExpr, NirFunction, NirParam, NirParamRole, NirStmt, NirValue, Terminator};

pub(crate) fn lower(
    resolved: &ResolvedAst,
    plan: &DerivedDisplayPlan,
    function: FuncId,
) -> NirFunction {
    let mut builder = FunctionBuilder::new(function, plan.method_name);
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|scope| scope.0);
    builder.function_type = plan.signature;
    builder.return_type = Intrinsic::Str.type_index();
    let receiver = builder.alloc_local();
    builder.params.push(NirParam {
        local: receiver,
        name: str_interner::intern("self"),
        type_index: plan.implementor,
        role: NirParamRole::User,
    });
    let entry = builder.new_block();
    builder.entry_block = entry;
    let result = builder.alloc_local();
    builder.blocks[entry.0 as usize].stmts.push(NirStmt::Assign(
        result,
        NirExpr::CallBuiltin(
            runtime::ids::DERIVED_DISPLAY,
            vec![NirValue::Local(receiver)],
        ),
    ));
    let checked = builder.alloc_local();
    builder.blocks[entry.0 as usize].stmts.push(NirStmt::Assign(
        checked,
        NirExpr::TypeAssert(NirValue::Local(result), Intrinsic::Str.type_index())
            .in_source_scope(resolved, plan.node),
    ));
    builder.blocks[entry.0 as usize].terminator = Terminator::Return(NirValue::Local(checked));
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use type_pool::{TypeId, TypeInfo, TypeKind};

    #[test]
    fn display_wrapper_checks_its_receiver_and_native_string_result() {
        let (mut resolved, node) = crate::literal::tests::resolved_integer("42", false, "i64");
        let owner = resolved.type_pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("Displayed"),
                fields: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 8,
        });
        let signature = resolved.type_pool.intern_structural(TypeKind::Function {
            params: vec![owner],
            ret: Intrinsic::Str.type_index(),
        });
        let plan = DerivedDisplayPlan {
            mode: resolution::DisplayDerivationMode::LegacyStruct,
            node,
            implementor: owner,
            trait_type: resolved.type_pool.well_known.display,
            method_name: str_interner::intern("to_string"),
            signature,
        };
        let function = lower(&resolved, &plan, FuncId(7));
        assert_eq!(function.function_type, signature);
        assert_eq!(function.params[0].type_index, owner);
        assert_eq!(
            function.entry_abi().parameters,
            vec![nsbc::ParameterAbi::Value]
        );
        let [
            NirStmt::Assign(result, native),
            NirStmt::Assign(checked, guard),
        ] = function.blocks[0].stmts.as_slice()
        else {
            panic!("native derivation must check the result before returning");
        };
        assert!(
            matches!(native, NirExpr::CallBuiltin(id, args) if *id == runtime::ids::DERIVED_DISPLAY && *args == vec![NirValue::Local(function.params[0].local)])
        );
        assert!(
            matches!(guard.unscoped(), NirExpr::TypeAssert(NirValue::Local(value), ty) if value == result && *ty == Intrinsic::Str.type_index())
        );
        assert!(
            matches!(function.blocks[0].terminator, Terminator::Return(NirValue::Local(value)) if value == *checked)
        );
    }
}
