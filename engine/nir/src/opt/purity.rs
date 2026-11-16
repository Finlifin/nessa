//! Exact purity and trap-safety classification of NIR expressions and builtins.

use runtime::{BuiltinFnId, ids};

use crate::{BinOp, NirExpr, NirStmt, NirValue, UnaryOp};

/// Pure and guaranteed never to trap or mutate/observe mutable state.
/// Safe to delete if unreferenced (DCE) and safe to hoist across loop guards (LICM).
pub(crate) fn is_pure_and_trap_free(expr: &NirExpr) -> bool {
    match expr.unscoped() {
        NirExpr::Use(_)
        | NirExpr::EnumIs(_, _, _)
        | NirExpr::ErrorIsOk(_)
        | NirExpr::TypeCheck(_, _)
        | NirExpr::NewClosure(_, _) => true,
        NirExpr::UnaryOp(UnaryOp::Not, NirValue::ConstBool(_)) => true,
        NirExpr::UnaryOp(UnaryOp::Neg, NirValue::ConstFloat(_)) => true,
        NirExpr::BinOp(op, NirValue::ConstFloat(_), NirValue::ConstFloat(_)) => matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
        ),
        NirExpr::BinOp(
            BinOp::And | BinOp::Or | BinOp::Eq | BinOp::Ne,
            NirValue::ConstBool(_),
            NirValue::ConstBool(_),
        ) => true,
        NirExpr::CallBuiltin(id, args) => {
            is_trap_free_pure_builtin(*id, args.len())
                || matches!(
                    (*id, args.as_slice()),
                    (
                        ids::STR_LEN | ids::STR_BYTE_COUNT | ids::STR_IS_EMPTY,
                        [NirValue::ConstStr(_)]
                    )
                )
        }
        _ => false,
    }
}

/// Allocations of fresh unescaped containers have no observable side effects when their
/// destination local is never read, so Dead Assignment Elimination may remove them
/// (though GVN/LICM must not merge distinct mutable allocations).
pub(crate) fn is_removable_when_unused(expr: &NirExpr, type_pool: &type_pool::TypePool) -> bool {
    if is_pure_and_trap_free(expr) {
        return true;
    }
    if let NirExpr::TypeCast(NirValue::ConstInt(v), ty) = expr.unscoped()
        && (0..=127).contains(v)
        && type_pool
            .canonical_type(*ty)
            .and_then(type_pool::Intrinsic::from_type_index)
            .is_some_and(|intr| {
                matches!(
                    intr,
                    type_pool::Intrinsic::I8
                        | type_pool::Intrinsic::I16
                        | type_pool::Intrinsic::I32
                        | type_pool::Intrinsic::I64
                        | type_pool::Intrinsic::I128
                        | type_pool::Intrinsic::Isize
                        | type_pool::Intrinsic::U8
                        | type_pool::Intrinsic::U16
                        | type_pool::Intrinsic::U32
                        | type_pool::Intrinsic::U64
                        | type_pool::Intrinsic::U128
                        | type_pool::Intrinsic::Usize
                        | type_pool::Intrinsic::F32
                        | type_pool::Intrinsic::F64
                )
            })
    {
        return true;
    }
    matches!(
        expr.unscoped(),
        NirExpr::NewObject(_, _)
            | NirExpr::NewList(_)
            | NirExpr::NewEnum(_, _, _)
            | NirExpr::ErrorOk(_, _)
            | NirExpr::ErrorErr(_, _)
    )
}

/// Expressions that depend only on their input operands (never mutable heap/globals)
/// and allocate no mutable identity, so if an identical expression already executed
/// without trapping in a dominating block, reusing the dominating local in dominated
/// blocks is 100% sound.
pub(crate) fn is_dominator_gvn_eligible(expr: &NirExpr) -> bool {
    match expr.unscoped() {
        NirExpr::Use(_)
        | NirExpr::BinOp(_, _, _)
        | NirExpr::UnaryOp(_, _)
        | NirExpr::EnumIs(_, _, _)
        | NirExpr::EnumField(_, _)
        | NirExpr::ErrorIsOk(_)
        | NirExpr::ErrorPayload(_)
        | NirExpr::TypeCheck(_, _)
        | NirExpr::TypeCast(_, _)
        | NirExpr::TypeAssert(_, _)
        | NirExpr::TraitProof(_, _)
        | NirExpr::TraitProject(_, _) => true,
        NirExpr::CallBuiltin(id, _) => is_pure_deterministic_builtin(*id),
        _ => false,
    }
}

fn is_trap_free_pure_builtin(id: BuiltinFnId, arity: usize) -> bool {
    match id {
        ids::PI
        | ids::E
        | ids::TAU
        | ids::LN_2
        | ids::I64_MIN
        | ids::I64_MAX
        | ids::U64_MAX
        | ids::I128_MIN
        | ids::I128_MAX
        | ids::U128_MAX
        | ids::CHAR_MIN
        | ids::CHAR_MAX
        | ids::CHAR_REPLACEMENT
        | ids::I64_BITS
        | ids::I128_BITS
        | ids::ISIZE_BITS
        | ids::U128_BITS
        | ids::USIZE_BITS
        | ids::F64_RADIX
        | ids::F64_MANTISSA_DIGITS
        | ids::F64_DIGITS
        | ids::F64_MIN_EXP
        | ids::F64_MAX_EXP
        | ids::F64_MIN_10_EXP
        | ids::F64_MAX_10_EXP => arity == 0,
        ids::TYPE_OF => arity == 1,
        _ => false,
    }
}

fn is_pure_deterministic_builtin(id: BuiltinFnId) -> bool {
    if is_trap_free_pure_builtin(id, 0) || id == ids::TYPE_OF {
        return true;
    }
    matches!(
        id,
        ids::STR_LEN
            | ids::STR_CHAR_LEN
            | ids::STR_BYTE_COUNT
            | ids::STR_IS_EMPTY
            | ids::STR_IS_ASCII
            | ids::STR_TRIM
            | ids::STR_TRIM_START
            | ids::STR_TRIM_END
            | ids::STR_TO_UPPER
            | ids::STR_TO_LOWER
            | ids::STR_CONCAT
            | ids::SIN
            | ids::COS
            | ids::TAN
            | ids::SQRT
            | ids::FLOOR
            | ids::CEIL
            | ids::ROUND
            | ids::TRUNC
            | ids::FRACT
            | ids::EXP
            | ids::LOG
            | ids::LOG2
            | ids::LOG10
            | ids::TO_I64
            | ids::TO_F64
    )
}

/// Whether `stmt` interacts with effect handlers, continuations, or runtime `defer` stacks
/// (disqualifying a function from inlining or self-tail-recursion elimination).
pub(crate) fn stmt_uses_handlers_or_defer(stmt: &NirStmt) -> bool {
    match stmt.unscoped() {
        NirStmt::PushHandler { .. } | NirStmt::PopHandler | NirStmt::AwaitPopHandler => true,
        NirStmt::Assign(_, expr) => matches!(
            expr.unscoped(),
            NirExpr::DelimitedCall { .. }
                | NirExpr::ResumeContinuation(_, _)
                | NirExpr::ResumeContinuationOnce(_, _)
                | NirExpr::EffectCall(_, _)
                | NirExpr::CallBuiltin(
                    ids::DEFER_PUSH | ids::DEFER_POP | ids::DEFER_RESUME_UNWIND,
                    _
                )
        ),
        _ => false,
    }
}
