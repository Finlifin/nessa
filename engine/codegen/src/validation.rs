//! Check encoding capacities before emission, including generated closures.

use nir::{NirExpr, NirModule, NirStmt, NirValue, Terminator};
use nsbc::FuncId;

#[derive(Debug)]
pub struct EncodingError {
    pub function: FuncId,
    pub message: String,
}

pub(crate) fn validate(module: &NirModule) -> Vec<EncodingError> {
    let mut errors = Vec::new();
    if module.globals.len() > (1 << 17) {
        errors.push(EncodingError {
            function: module.entry.unwrap_or(FuncId(0)),
            message: "an artifact currently supports at most 131072 global bindings".into(),
        });
    }
    for function in &module.functions {
        let mut report = |message: &str| {
            errors.push(EncodingError {
                function: function.func_id,
                message: message.into(),
            })
        };
        if function.blocks.is_empty()
            || function.entry_block.0 != 0
            || function
                .blocks
                .iter()
                .enumerate()
                .any(|(index, block)| block.id.0 as usize != index)
        {
            report("NIR blocks must be dense from zero with entry block zero");
        }
        if function.local_count > (1 << 17) {
            report("a function currently supports at most 131072 local value slots");
        }
        if function.entry_scope.is_none()
            && function.params.iter().any(|parameter| {
                parameter.type_index != type_pool::TypeIndex::INVALID
                    && parameter.type_index != type_pool::Intrinsic::Any.type_index()
            })
        {
            report("typed parameter checks require the function declaration scope");
        }
        let mut seen_user_parameter = false;
        for parameter in &function.params {
            match parameter.role {
                nir::NirParamRole::User
                | nir::NirParamRole::TraitProof { .. }
                | nir::NirParamRole::TraitSelfProof { .. } => seen_user_parameter = true,
                nir::NirParamRole::Capture | nir::NirParamRole::CaptureProof { .. }
                    if seen_user_parameter || !function.is_closure =>
                {
                    report("NIR captures must form a closure's parameter prefix");
                }
                nir::NirParamRole::Capture | nir::NirParamRole::CaptureProof { .. } => {}
            }
        }
        for (index, parameter) in function.params.iter().enumerate() {
            match parameter.role {
                nir::NirParamRole::TraitProof { view }
                | nir::NirParamRole::TraitSelfProof { view } => {
                    if !function
                        .params
                        .get(index + 1)
                        .is_some_and(|next| next.role == nir::NirParamRole::User)
                    {
                        report("trait proof must precede its user data parameter");
                    }
                    if view.as_u32() >= 1 << 12 || function.entry_scope.is_none() {
                        report(
                            "trait parameter check requires encodable view and declaration scope",
                        );
                    }
                }
                nir::NirParamRole::CaptureProof { view } => {
                    if index == 0 || function.params[index - 1].role != nir::NirParamRole::Capture {
                        report("capture proof must follow its data capture");
                    }
                    if view.as_u32() >= 1 << 12 || function.entry_scope.is_none() {
                        report("capture proof check requires encodable view and declaration scope");
                    }
                }
                _ => {}
            }
        }
        if function.params.len() > 32 {
            report("a function currently supports at most 32 parameters including captures");
        }
        if function.params.iter().any(|param| {
            param.type_index != type_pool::TypeIndex::INVALID
                && param.type_index.as_u32() >= (1 << 12)
        }) {
            report("parameter checks currently support only 12-bit type indices");
        }
        if function
            .params
            .iter()
            .any(|parameter| parameter.local.0 >= function.local_count)
        {
            report("NIR parameter references a local outside the function's slot layout");
        }
        for block in &function.blocks {
            let target_is_invalid =
                |target: nir::BlockId| target.0 as usize >= function.blocks.len();
            if match &block.terminator {
                Terminator::Goto(target) => target_is_invalid(*target),
                Terminator::Branch(_, first, second) => {
                    target_is_invalid(*first) || target_is_invalid(*second)
                }
                _ => false,
            } {
                report("NIR jump references a block outside the function");
            }
            let value = match &block.terminator {
                Terminator::Return(value) | Terminator::Branch(value, _, _) => Some(*value),
                _ => None,
            };
            if value.is_some_and(|value| invalid_local(value, function.local_count)) {
                report("NIR terminator references a local outside the function's slot layout");
            }
        }
        for statement in function.blocks.iter().flat_map(|block| &block.stmts) {
            match statement {
                NirStmt::Scoped { statement, .. } if !statement.requires_scope() => {
                    report("scoped statement must contain a checked store")
                }
                statement if statement.requires_scope() => {
                    report("checked store is missing its lexical scope")
                }
                _ => {}
            }
            let statement = statement.unscoped();
            let invalid_statement = match statement {
                NirStmt::StoreField(object, index, value) => {
                    if *index >= (1 << 12) {
                        report("NIR field store index exceeds its encoding capacity");
                    }
                    invalid_local(*object, function.local_count)
                        || invalid_local(*value, function.local_count)
                }
                NirStmt::StoreIndex(object, index, value) => [object, index, value]
                    .iter()
                    .any(|value| invalid_local(**value, function.local_count)),
                NirStmt::StoreGlobal(global, value) => {
                    if global.0 >= (1 << 17) || global.0 as usize >= module.globals.len() {
                        report(
                            "NIR global store references an index outside its schema or encoding capacity",
                        );
                    }
                    invalid_local(*value, function.local_count)
                }
                NirStmt::Drop(local) => local.0 >= function.local_count,
                NirStmt::PushHandler { closure, .. } => {
                    invalid_local(*closure, function.local_count)
                }
                NirStmt::EffectCall {
                    evidence,
                    result,
                    args,
                    ..
                } => {
                    evidence.0 >= function.local_count
                        || result.0 >= function.local_count
                        || args
                            .iter()
                            .any(|&value| invalid_local(value, function.local_count))
                }
                _ => false,
            };
            if invalid_statement {
                report("NIR statement references a local outside the function's slot layout");
            }
            let NirStmt::Assign(local, expression) = statement else {
                continue;
            };
            if local.0 >= function.local_count || invalid_operands(expression, function.local_count)
            {
                report("NIR expression references a local outside the function's slot layout");
            }
            match expression {
                NirExpr::ScopedCall { call, .. } if !call.requires_scope() => {
                    report("scoped expression must contain a checked query")
                }
                expression if expression.requires_scope() => {
                    report("checked expression is missing its lexical scope")
                }
                _ => {}
            }
            match expression.unscoped() {
                NirExpr::NewList(length) if *length >= (1 << 12) => {
                    report("a list allocation instruction supports only 12-bit initial lengths")
                }
                NirExpr::LoadGlobal(global)
                    if global.0 >= (1 << 17) || global.0 as usize >= module.globals.len() =>
                {
                    report(
                        "NIR global load references an index outside its schema or encoding capacity",
                    );
                }
                NirExpr::NewObject(ty, _)
                | NirExpr::TypeCheck(_, ty)
                | NirExpr::TypeCast(_, ty)
                | NirExpr::TypeAssert(_, ty)
                | NirExpr::TraitProof(_, ty)
                | NirExpr::TraitAssert(_, _, ty)
                | NirExpr::TraitProject(_, ty)
                    if ty.as_u32() >= (1 << 12) =>
                {
                    report("this instruction currently supports only 12-bit type indices")
                }
                NirExpr::FieldAccess(_, index) | NirExpr::EnumField(_, index)
                    if *index >= (1 << 12) =>
                {
                    report("a field instruction currently supports only 12-bit field indices")
                }
                NirExpr::NewObject(_, fields) if fields.len() > (1 << 12) => {
                    report("an object constructor currently supports at most 4096 fields")
                }
                NirExpr::NewClosure(_, captures) if captures.len() > 31 => {
                    report("a closure currently supports at most 31 captures")
                }
                NirExpr::Call(_, args)
                | NirExpr::CallWithProof(_, args)
                | NirExpr::CallWithProof(_, args)
                | NirExpr::CallBuiltin(_, args)
                    if args.len() > 32 =>
                {
                    report("a call currently supports at most 32 arguments")
                }
                NirExpr::CallIndirect(_, args)
                | NirExpr::CallIndirectProof(_, args)
                | NirExpr::MethodCall(_, _, args)
                    if args.len() > 31 =>
                {
                    report("an indirect or method call currently supports at most 31 arguments")
                }
                NirExpr::TraitCall {
                    args, slot, view, ..
                } if args.len() > 30 || *slot >= 1 << 12 || view.as_u32() >= 1 << 12 => {
                    report(
                        "trait call exceeds its receiver, proof, slot or view encoding capacity",
                    );
                }
                NirExpr::EffectCall(_, args) if args.len() > 31 => {
                    report("an effect call currently supports at most 31 arguments")
                }
                _ => {}
            }
        }
    }
    errors
}

fn invalid_local(value: NirValue, count: u32) -> bool {
    matches!(value, NirValue::Local(local) if local.0 >= count)
}

fn invalid_operands(expression: &NirExpr, count: u32) -> bool {
    match expression {
        NirExpr::ScopedCall { call, .. } => invalid_operands(call, count),
        NirExpr::LoadGlobal(_) | NirExpr::NewList(_) => false,
        NirExpr::Use(value)
        | NirExpr::NewEnum(_, _, value)
        | NirExpr::EnumIs(value, _, _)
        | NirExpr::EnumField(value, _)
        | NirExpr::ErrorOk(value, _)
        | NirExpr::ErrorErr(value, _)
        | NirExpr::ErrorIsOk(value)
        | NirExpr::ErrorPayload(value)
        | NirExpr::UnaryOp(_, value)
        | NirExpr::FieldAccess(value, _)
        | NirExpr::TypeCheck(value, _)
        | NirExpr::TypeCast(value, _)
        | NirExpr::TypeAssert(value, _)
        | NirExpr::TraitProof(value, _)
        | NirExpr::TraitProject(value, _) => invalid_local(*value, count),
        NirExpr::TraitAssert(value, proof, _) => {
            invalid_local(*value, count) || invalid_local(*proof, count)
        }
        NirExpr::TraitCall {
            receiver,
            proof,
            args,
            ..
        } => {
            invalid_local(*receiver, count)
                || invalid_local(*proof, count)
                || args.iter().any(|&value| invalid_local(value, count))
        }
        NirExpr::DelimitedCall { body, .. } => invalid_local(*body, count),
        NirExpr::BinOp(_, left, right)
        | NirExpr::IndexAccess(left, right)
        | NirExpr::ResumeContinuation(left, right)
        | NirExpr::ResumeContinuationOnce(left, right) => {
            invalid_local(*left, count) || invalid_local(*right, count)
        }
        NirExpr::Call(_, args)
        | NirExpr::CallWithProof(_, args)
        | NirExpr::CallBuiltin(_, args)
        | NirExpr::EffectCall(_, args)
        | NirExpr::NewClosure(_, args)
        | NirExpr::NewObject(_, args) => args.iter().any(|&value| invalid_local(value, count)),
        NirExpr::CallIndirect(receiver, args)
        | NirExpr::CallIndirectProof(receiver, args)
        | NirExpr::MethodCall(receiver, _, args) => {
            invalid_local(*receiver, count) || args.iter().any(|&value| invalid_local(value, count))
        }
    }
}
