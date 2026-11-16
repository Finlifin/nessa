//! Checked scalar constant folding, tag/type-check folding, and algebraic identity
//! simplification.
//!
//! Preserves all runtime trap behaviors (`NumericOverflow`, `DivisionByZero`, `TypeError`)
//! and narrow scalar runtime tags (`i8`/`i16`/`i32`/`u8`/`u16`/`u32`/`usize`/`f32`).

use std::collections::HashMap;

use runtime::ids;
use type_pool::{Intrinsic, TypeIndex, TypePool};

use crate::opt::{nir_value_bits_eq, split_scoped_expr, wrap_expr_preserving_scope};
use crate::{BinOp, NirExpr, NirFunction, NirLocal, NirStmt, NirValue, UnaryOp};

/// Fold constant `NirExpr`s and safe algebraic identities across all blocks in `func`.
pub(crate) fn fold_constants_and_identities(func: &mut NirFunction, type_pool: &TypePool) {
    let known_types = collect_proven_local_types(func, type_pool);
    let known_consts = collect_single_def_constants(func);

    for block in &mut func.blocks {
        for stmt in &mut block.stmts {
            let NirStmt::Assign(_, expr) = stmt.unscoped_mut() else {
                continue;
            };
            let taken = std::mem::replace(expr, NirExpr::Use(NirValue::Unit));
            let (inner, scope) = split_scoped_expr(taken);
            let simplified = simplify_expr(inner, type_pool, &known_types, &known_consts);
            *expr = wrap_expr_preserving_scope(simplified, scope);
        }
    }
}

/// Evaluate `expr` on known `NirValue` operands when the result is a compile-time `NirValue`
/// and evaluation cannot trap at runtime. Shared by `const_fold` and `sccp`.
pub(crate) fn try_eval_const_expr(expr: &NirExpr, type_pool: &TypePool) -> Option<NirValue> {
    match expr.unscoped() {
        NirExpr::Use(v) if !matches!(v, NirValue::Local(_)) => Some(*v),
        NirExpr::UnaryOp(op, v) => eval_unary(*op, *v),
        NirExpr::BinOp(op, l, r) => eval_binary(*op, *l, *r),
        NirExpr::EnumIs(NirValue::ConstEnum(actual_ty, actual_tag), target_ty, target_tag) => {
            let ac = type_pool.canonical_type(*actual_ty)?;
            let tc = type_pool.canonical_type(*target_ty)?;
            Some(NirValue::ConstBool(ac == tc && actual_tag == target_tag))
        }
        NirExpr::EnumIs(
            NirValue::ConstInt(_)
            | NirValue::ConstUInt(_)
            | NirValue::ConstFloat(_)
            | NirValue::ConstBool(_)
            | NirValue::ConstChar(_)
            | NirValue::ConstStr(_)
            | NirValue::Null
            | NirValue::Unit,
            _,
            _,
        ) => Some(NirValue::ConstBool(false)),
        NirExpr::TypeCheck(val, target_ty) => eval_type_check(*val, *target_ty, type_pool),
        NirExpr::TypeCast(val, target_ty) => eval_type_cast(*val, *target_ty, type_pool),
        NirExpr::TypeAssert(val, target_ty) => eval_type_assert(*val, *target_ty, type_pool),
        NirExpr::CallBuiltin(id, args) if args.is_empty() => match *id {
            ids::PI => Some(NirValue::ConstFloat(std::f64::consts::PI)),
            ids::E => Some(NirValue::ConstFloat(std::f64::consts::E)),
            ids::TAU => Some(NirValue::ConstFloat(std::f64::consts::TAU)),
            ids::LN_2 => Some(NirValue::ConstFloat(std::f64::consts::LN_2)),
            ids::I64_MIN => Some(NirValue::ConstInt(i64::MIN)),
            ids::I64_MAX => Some(NirValue::ConstInt(i64::MAX)),
            ids::U64_MAX => Some(NirValue::ConstUInt(u64::MAX)),
            ids::I128_MIN => Some(NirValue::ConstI128(i128::MIN)),
            ids::I128_MAX => Some(NirValue::ConstI128(i128::MAX)),
            ids::U128_MAX => Some(NirValue::ConstU128(u128::MAX)),
            ids::I64_BITS => Some(NirValue::ConstInt(64)),
            ids::I128_BITS | ids::U128_BITS => Some(NirValue::ConstInt(128)),
            _ => None,
        },
        NirExpr::CallBuiltin(ids::STR_LEN | ids::STR_BYTE_COUNT, args) => match args.as_slice() {
            [NirValue::ConstStr(sid)] => {
                Some(NirValue::ConstInt(str_interner::get(*sid).len() as i64))
            }
            _ => None,
        },
        NirExpr::CallBuiltin(ids::STR_IS_EMPTY, args) => match args.as_slice() {
            [NirValue::ConstStr(sid)] => {
                Some(NirValue::ConstBool(str_interner::get(*sid).is_empty()))
            }
            _ => None,
        },
        _ => None,
    }
}

fn eval_unary(op: UnaryOp, v: NirValue) -> Option<NirValue> {
    match (op, v) {
        (UnaryOp::Neg, NirValue::ConstInt(x)) => x.checked_neg().map(NirValue::ConstInt),
        (UnaryOp::Neg, NirValue::ConstI128(x)) => x.checked_neg().map(NirValue::ConstI128),
        (UnaryOp::Neg, NirValue::ConstFloat(x)) => Some(NirValue::ConstFloat(-x)),
        (UnaryOp::Not, NirValue::ConstBool(b)) => Some(NirValue::ConstBool(!b)),
        (UnaryOp::BitNot, NirValue::ConstInt(x)) => Some(NirValue::ConstInt(!x)),
        (UnaryOp::BitNot, NirValue::ConstUInt(x)) => Some(NirValue::ConstUInt(!x)),
        _ => None,
    }
}

fn eval_binary(op: BinOp, l: NirValue, r: NirValue) -> Option<NirValue> {
    match (l, r) {
        (NirValue::ConstInt(a), NirValue::ConstInt(b)) => match op {
            BinOp::Add => a.checked_add(b).map(NirValue::ConstInt),
            BinOp::Sub => a.checked_sub(b).map(NirValue::ConstInt),
            BinOp::Mul => a.checked_mul(b).map(NirValue::ConstInt),
            BinOp::Div if b != 0 => a.checked_div(b).map(NirValue::ConstInt),
            BinOp::Mod if b != 0 => a.checked_rem(b).map(NirValue::ConstInt),
            BinOp::BitAnd => Some(NirValue::ConstInt(a & b)),
            BinOp::BitOr => Some(NirValue::ConstInt(a | b)),
            BinOp::BitXor => Some(NirValue::ConstInt(a ^ b)),
            BinOp::Eq => Some(NirValue::ConstBool(a == b)),
            BinOp::Ne => Some(NirValue::ConstBool(a != b)),
            BinOp::Lt => Some(NirValue::ConstBool(a < b)),
            BinOp::Le => Some(NirValue::ConstBool(a <= b)),
            BinOp::Gt => Some(NirValue::ConstBool(a > b)),
            BinOp::Ge => Some(NirValue::ConstBool(a >= b)),
            _ => None,
        },
        (NirValue::ConstUInt(a), NirValue::ConstUInt(b)) => match op {
            BinOp::Add => a.checked_add(b).map(NirValue::ConstUInt),
            BinOp::Sub => a.checked_sub(b).map(NirValue::ConstUInt),
            BinOp::Mul => a.checked_mul(b).map(NirValue::ConstUInt),
            BinOp::Div if b != 0 => a.checked_div(b).map(NirValue::ConstUInt),
            BinOp::Mod if b != 0 => a.checked_rem(b).map(NirValue::ConstUInt),
            BinOp::BitAnd => Some(NirValue::ConstUInt(a & b)),
            BinOp::BitOr => Some(NirValue::ConstUInt(a | b)),
            BinOp::BitXor => Some(NirValue::ConstUInt(a ^ b)),
            BinOp::Eq => Some(NirValue::ConstBool(a == b)),
            BinOp::Ne => Some(NirValue::ConstBool(a != b)),
            BinOp::Lt => Some(NirValue::ConstBool(a < b)),
            BinOp::Le => Some(NirValue::ConstBool(a <= b)),
            BinOp::Gt => Some(NirValue::ConstBool(a > b)),
            BinOp::Ge => Some(NirValue::ConstBool(a >= b)),
            _ => None,
        },
        (NirValue::ConstI128(a), NirValue::ConstI128(b)) => match op {
            BinOp::Add => a.checked_add(b).map(NirValue::ConstI128),
            BinOp::Sub => a.checked_sub(b).map(NirValue::ConstI128),
            BinOp::Mul => a.checked_mul(b).map(NirValue::ConstI128),
            BinOp::Div if b != 0 => a.checked_div(b).map(NirValue::ConstI128),
            BinOp::Mod if b != 0 => a.checked_rem(b).map(NirValue::ConstI128),
            BinOp::Eq => Some(NirValue::ConstBool(a == b)),
            BinOp::Ne => Some(NirValue::ConstBool(a != b)),
            BinOp::Lt => Some(NirValue::ConstBool(a < b)),
            BinOp::Le => Some(NirValue::ConstBool(a <= b)),
            BinOp::Gt => Some(NirValue::ConstBool(a > b)),
            BinOp::Ge => Some(NirValue::ConstBool(a >= b)),
            _ => None,
        },
        (NirValue::ConstU128(a), NirValue::ConstU128(b)) => match op {
            BinOp::Add => a.checked_add(b).map(NirValue::ConstU128),
            BinOp::Sub => a.checked_sub(b).map(NirValue::ConstU128),
            BinOp::Mul => a.checked_mul(b).map(NirValue::ConstU128),
            BinOp::Div if b != 0 => a.checked_div(b).map(NirValue::ConstU128),
            BinOp::Mod if b != 0 => a.checked_rem(b).map(NirValue::ConstU128),
            BinOp::Eq => Some(NirValue::ConstBool(a == b)),
            BinOp::Ne => Some(NirValue::ConstBool(a != b)),
            BinOp::Lt => Some(NirValue::ConstBool(a < b)),
            BinOp::Le => Some(NirValue::ConstBool(a <= b)),
            BinOp::Gt => Some(NirValue::ConstBool(a > b)),
            BinOp::Ge => Some(NirValue::ConstBool(a >= b)),
            _ => None,
        },
        (NirValue::ConstFloat(a), NirValue::ConstFloat(b)) => match op {
            BinOp::Add => Some(NirValue::ConstFloat(a + b)),
            BinOp::Sub => Some(NirValue::ConstFloat(a - b)),
            BinOp::Mul => Some(NirValue::ConstFloat(a * b)),
            BinOp::Div => Some(NirValue::ConstFloat(a / b)),
            BinOp::Mod => Some(NirValue::ConstFloat(a % b)),
            BinOp::Eq => Some(NirValue::ConstBool(a == b)),
            BinOp::Ne => Some(NirValue::ConstBool(a != b)),
            BinOp::Lt => Some(NirValue::ConstBool(a < b)),
            BinOp::Le => Some(NirValue::ConstBool(a <= b)),
            BinOp::Gt => Some(NirValue::ConstBool(a > b)),
            BinOp::Ge => Some(NirValue::ConstBool(a >= b)),
            _ => None,
        },
        (NirValue::ConstBool(a), NirValue::ConstBool(b)) => match op {
            BinOp::And => Some(NirValue::ConstBool(a && b)),
            BinOp::Or => Some(NirValue::ConstBool(a || b)),
            BinOp::Eq => Some(NirValue::ConstBool(a == b)),
            BinOp::Ne => Some(NirValue::ConstBool(a != b)),
            _ => None,
        },
        (NirValue::ConstChar(a), NirValue::ConstChar(b)) => match op {
            BinOp::Eq => Some(NirValue::ConstBool(a == b)),
            BinOp::Ne => Some(NirValue::ConstBool(a != b)),
            BinOp::Lt => Some(NirValue::ConstBool(a < b)),
            BinOp::Le => Some(NirValue::ConstBool(a <= b)),
            BinOp::Gt => Some(NirValue::ConstBool(a > b)),
            BinOp::Ge => Some(NirValue::ConstBool(a >= b)),
            _ => None,
        },
        (NirValue::Null, NirValue::Null) | (NirValue::Unit, NirValue::Unit) => match op {
            BinOp::Eq => Some(NirValue::ConstBool(true)),
            BinOp::Ne => Some(NirValue::ConstBool(false)),
            _ => None,
        },
        (NirValue::Null, other) | (other, NirValue::Null)
            if !matches!(other, NirValue::Local(_)) =>
        {
            match op {
                BinOp::Eq => Some(NirValue::ConstBool(false)),
                BinOp::Ne => Some(NirValue::ConstBool(true)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn const_runtime_type(val: NirValue, pool: &TypePool) -> Option<TypeIndex> {
    match val {
        NirValue::ConstInt(_) => Some(Intrinsic::I64.type_index()),
        NirValue::ConstUInt(_) => Some(Intrinsic::U64.type_index()),
        NirValue::ConstI128(_) => Some(Intrinsic::I128.type_index()),
        NirValue::ConstU128(_) => Some(Intrinsic::U128.type_index()),
        NirValue::ConstFloat(_) => Some(Intrinsic::F64.type_index()),
        NirValue::ConstBool(_) => Some(Intrinsic::Bool.type_index()),
        NirValue::ConstChar(_) => Some(Intrinsic::Char.type_index()),
        NirValue::ConstStr(_) => Some(Intrinsic::Str.type_index()),
        NirValue::ConstType(_) => Some(Intrinsic::Type.type_index()),
        NirValue::ConstEnum(ty, _) => pool.canonical_type(ty),
        NirValue::Unit => Some(Intrinsic::Unit.type_index()),
        NirValue::Null => Some(pool.null_type()),
        NirValue::Local(_) => None,
    }
}

fn eval_type_check(val: NirValue, target_ty: TypeIndex, pool: &TypePool) -> Option<NirValue> {
    let target = pool.canonical_type(target_ty)?;
    if matches!(pool.get(target).kind, type_pool::TypeKind::Trait { .. }) {
        return None;
    }
    let actual = const_runtime_type(val, pool)?;
    Some(NirValue::ConstBool(pool.is_subtype(actual, target)))
}

fn eval_type_cast(val: NirValue, target_ty: TypeIndex, pool: &TypePool) -> Option<NirValue> {
    let intr = pool.as_intrinsic(target_ty)?;
    // Only fold to `NirValue` constants whose runtime representation matches `intr` exactly
    // (preserving narrow `i8`/`i16`/`i32`/`u8`/`u16`/`u32`/`usize`/`f32` tags!).
    match (val, intr) {
        (_, Intrinsic::Any) => (!matches!(val, NirValue::Local(_))).then_some(val),
        (NirValue::ConstInt(v), Intrinsic::I64) => Some(NirValue::ConstInt(v)),
        (NirValue::ConstInt(v), Intrinsic::U64) if v >= 0 => Some(NirValue::ConstUInt(v as u64)),
        (NirValue::ConstInt(v), Intrinsic::I128) => Some(NirValue::ConstI128(v as i128)),
        (NirValue::ConstInt(v), Intrinsic::U128) if v >= 0 => Some(NirValue::ConstU128(v as u128)),
        (NirValue::ConstInt(v), Intrinsic::F64) => Some(NirValue::ConstFloat(v as f64)),
        (NirValue::ConstUInt(v), Intrinsic::U64) => Some(NirValue::ConstUInt(v)),
        (NirValue::ConstUInt(v), Intrinsic::I64) if v <= i64::MAX as u64 => {
            Some(NirValue::ConstInt(v as i64))
        }
        (NirValue::ConstFloat(v), Intrinsic::F64) => Some(NirValue::ConstFloat(v)),
        (NirValue::ConstBool(b), Intrinsic::Bool) => Some(NirValue::ConstBool(b)),
        (NirValue::ConstChar(c), Intrinsic::Char) => Some(NirValue::ConstChar(c)),
        (NirValue::ConstStr(s), Intrinsic::Str) => Some(NirValue::ConstStr(s)),
        _ => None,
    }
}

fn eval_type_assert(val: NirValue, target_ty: TypeIndex, pool: &TypePool) -> Option<NirValue> {
    let target = pool.canonical_type(target_ty)?;
    if pool.as_intrinsic(target) == Some(Intrinsic::Any) {
        return (!matches!(val, NirValue::Local(_))).then_some(val);
    }
    if matches!(pool.get(target).kind, type_pool::TypeKind::Trait { .. }) {
        return None;
    }
    let actual = const_runtime_type(val, pool)?;
    if actual == target {
        return Some(val);
    }
    // Optional target `?T` where `val` is `Null` or exact `T`.
    if let type_pool::TypeKind::Optional { inner } = pool.get(target).kind
        && (val == NirValue::Null || pool.canonical_type(inner) == Some(actual))
    {
        return Some(val);
    }
    None
}

fn simplify_expr(
    expr: NirExpr,
    pool: &TypePool,
    known_types: &HashMap<NirLocal, TypeIndex>,
    known_consts: &HashMap<NirLocal, NirValue>,
) -> NirExpr {
    if let Some(c) = try_eval_const_expr(&expr, pool) {
        return NirExpr::Use(c);
    }
    match expr {
        NirExpr::TypeAssert(NirValue::Local(loc), target_ty)
        | NirExpr::TypeCast(NirValue::Local(loc), target_ty) => {
            if let Some(canon_target) = pool.canonical_type(target_ty) {
                if pool.as_intrinsic(canon_target) == Some(Intrinsic::Any) {
                    return NirExpr::Use(NirValue::Local(loc));
                }
                if let Some(&loc_ty) = known_types.get(&loc)
                    && pool.canonical_type(loc_ty) == Some(canon_target)
                    && !matches!(
                        pool.get(canon_target).kind,
                        type_pool::TypeKind::Trait { .. }
                    )
                {
                    return NirExpr::Use(NirValue::Local(loc));
                }
            }
            expr
        }
        NirExpr::TypeCheck(NirValue::Local(loc), target_ty) => {
            if let Some(canon_target) = pool.canonical_type(target_ty)
                && !matches!(
                    pool.get(canon_target).kind,
                    type_pool::TypeKind::Trait { .. }
                )
                && let Some(&loc_ty) = known_types.get(&loc)
                && let Some(canon_loc) = pool.canonical_type(loc_ty)
                && pool.as_intrinsic(canon_loc) != Some(Intrinsic::Any)
                && pool.is_subtype(canon_loc, canon_target)
            {
                return NirExpr::Use(NirValue::ConstBool(true));
            }
            expr
        }
        NirExpr::BinOp(op, l, r) => {
            let l_is_i64 = is_proven_intrinsic(l, Intrinsic::I64, pool, known_types);
            let r_is_i64 = is_proven_intrinsic(r, Intrinsic::I64, pool, known_types);
            let l_is_bool = is_proven_intrinsic(l, Intrinsic::Bool, pool, known_types);
            let r_is_bool = is_proven_intrinsic(r, Intrinsic::Bool, pool, known_types);
            let l_intr = match l {
                NirValue::Local(loc) => known_types
                    .get(&loc)
                    .and_then(|&ty| pool.canonical_type(ty))
                    .and_then(|c| pool.as_intrinsic(c)),
                other => const_runtime_type(other, pool).and_then(|c| pool.as_intrinsic(c)),
            };
            let l_is_reflexive_ord = matches!(
                l_intr,
                Some(
                    Intrinsic::I8
                        | Intrinsic::I16
                        | Intrinsic::I32
                        | Intrinsic::I64
                        | Intrinsic::I128
                        | Intrinsic::Isize
                        | Intrinsic::U8
                        | Intrinsic::U16
                        | Intrinsic::U32
                        | Intrinsic::U64
                        | Intrinsic::U128
                        | Intrinsic::Usize
                        | Intrinsic::Char
                )
            );
            let l_is_reflexive_eq =
                l_is_reflexive_ord || matches!(l_intr, Some(Intrinsic::Bool | Intrinsic::Str));
            match (op, l, r) {
                (
                    BinOp::Add | BinOp::Sub | BinOp::BitOr | BinOp::BitXor,
                    x,
                    NirValue::ConstInt(0),
                ) if l_is_i64 => NirExpr::Use(x),
                (BinOp::Add | BinOp::BitOr | BinOp::BitXor, NirValue::ConstInt(0), x)
                    if r_is_i64 =>
                {
                    NirExpr::Use(x)
                }
                (BinOp::Mul, x, NirValue::ConstInt(1)) if l_is_i64 => NirExpr::Use(x),
                (BinOp::Mul, NirValue::ConstInt(1), x) if r_is_i64 => NirExpr::Use(x),
                (BinOp::Div, x, NirValue::ConstInt(1)) if l_is_i64 => NirExpr::Use(x),
                (BinOp::Sub | BinOp::BitXor, x, y) if l_is_i64 && nir_value_bits_eq(&x, &y) => {
                    NirExpr::Use(NirValue::ConstInt(0))
                }
                (BinOp::Eq, x, y) if l_is_reflexive_eq && nir_value_bits_eq(&x, &y) => {
                    NirExpr::Use(NirValue::ConstBool(true))
                }
                (BinOp::Ne, x, y) if l_is_reflexive_eq && nir_value_bits_eq(&x, &y) => {
                    NirExpr::Use(NirValue::ConstBool(false))
                }
                (BinOp::Lt | BinOp::Gt, x, y)
                    if l_is_reflexive_ord && nir_value_bits_eq(&x, &y) =>
                {
                    NirExpr::Use(NirValue::ConstBool(false))
                }
                (BinOp::Le | BinOp::Ge, x, y)
                    if l_is_reflexive_ord && nir_value_bits_eq(&x, &y) =>
                {
                    NirExpr::Use(NirValue::ConstBool(true))
                }
                (BinOp::Eq, x, NirValue::ConstBool(true))
                | (BinOp::And, x, NirValue::ConstBool(true))
                | (BinOp::Or, x, NirValue::ConstBool(false))
                    if l_is_bool =>
                {
                    NirExpr::Use(x)
                }
                (BinOp::Eq, NirValue::ConstBool(true), x)
                | (BinOp::And, NirValue::ConstBool(true), x)
                | (BinOp::Or, NirValue::ConstBool(false), x)
                    if r_is_bool =>
                {
                    NirExpr::Use(x)
                }
                (BinOp::Eq, x, NirValue::ConstBool(false))
                | (BinOp::Ne, x, NirValue::ConstBool(true))
                    if l_is_bool =>
                {
                    NirExpr::UnaryOp(UnaryOp::Not, x)
                }
                _ => {
                    // Check if substituting single-def constants enables constant folding.
                    let l_sub = resolve_const_operand(l, known_consts);
                    let r_sub = resolve_const_operand(r, known_consts);
                    if let Some(c) = eval_binary(op, l_sub, r_sub) {
                        NirExpr::Use(c)
                    } else {
                        NirExpr::BinOp(op, l, r)
                    }
                }
            }
        }
        other => other,
    }
}

fn resolve_const_operand(v: NirValue, known_consts: &HashMap<NirLocal, NirValue>) -> NirValue {
    match v {
        NirValue::Local(loc) => known_consts.get(&loc).copied().unwrap_or(v),
        other => other,
    }
}

fn is_proven_intrinsic(
    v: NirValue,
    intr: Intrinsic,
    pool: &TypePool,
    known_types: &HashMap<NirLocal, TypeIndex>,
) -> bool {
    match v {
        NirValue::ConstInt(_) => intr == Intrinsic::I64,
        NirValue::ConstUInt(_) => intr == Intrinsic::U64,
        NirValue::ConstFloat(_) => intr == Intrinsic::F64,
        NirValue::ConstBool(_) => intr == Intrinsic::Bool,
        NirValue::Local(loc) => {
            known_types.get(&loc).and_then(|&ty| pool.as_intrinsic(ty)) == Some(intr)
        }
        _ => false,
    }
}

/// Collect locals that are assigned at most once (or are checked function parameters)
/// and whose exact runtime type is statically guaranteed on every write.
pub(crate) fn collect_proven_local_types(
    func: &NirFunction,
    pool: &TypePool,
) -> HashMap<NirLocal, TypeIndex> {
    let mut assign_counts: HashMap<NirLocal, usize> = HashMap::new();
    for param in &func.params {
        *assign_counts.entry(param.local).or_default() += 1;
    }
    for block in &func.blocks {
        for stmt in &block.stmts {
            if let NirStmt::Assign(dst, _) = stmt.unscoped() {
                *assign_counts.entry(*dst).or_default() += 1;
            }
        }
    }

    let mut out = HashMap::new();
    for param in &func.params {
        if assign_counts.get(&param.local).copied() == Some(1)
            && param.type_index != TypeIndex::INVALID
            && pool.as_intrinsic(param.type_index) != Some(Intrinsic::Any)
        {
            out.insert(param.local, param.type_index);
        }
    }
    for block in &func.blocks {
        for stmt in &block.stmts {
            let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
                continue;
            };
            if assign_counts.get(dst).copied() != Some(1) {
                continue;
            }
            let inferred = match expr.unscoped() {
                NirExpr::Use(v) => const_runtime_type(*v, pool),
                NirExpr::TypeCast(_, ty) | NirExpr::TypeAssert(_, ty) => pool.canonical_type(*ty),
                NirExpr::EnumIs(_, _, _)
                | NirExpr::ErrorIsOk(_)
                | NirExpr::TypeCheck(_, _)
                | NirExpr::BinOp(
                    BinOp::Eq
                    | BinOp::Ne
                    | BinOp::Lt
                    | BinOp::Le
                    | BinOp::Gt
                    | BinOp::Ge
                    | BinOp::And
                    | BinOp::Or,
                    _,
                    _,
                )
                | NirExpr::UnaryOp(UnaryOp::Not, _) => Some(Intrinsic::Bool.type_index()),
                NirExpr::BinOp(
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod,
                    l,
                    r,
                ) => {
                    if is_proven_intrinsic(*l, Intrinsic::I64, pool, &out)
                        && is_proven_intrinsic(*r, Intrinsic::I64, pool, &out)
                    {
                        Some(Intrinsic::I64.type_index())
                    } else if is_proven_intrinsic(*l, Intrinsic::F64, pool, &out)
                        && is_proven_intrinsic(*r, Intrinsic::F64, pool, &out)
                    {
                        Some(Intrinsic::F64.type_index())
                    } else {
                        None
                    }
                }
                NirExpr::CallBuiltin(ids::LIST_LEN | ids::MAP_LEN | ids::STR_LEN, _) => {
                    Some(Intrinsic::I64.type_index())
                }
                NirExpr::NewObject(ty, _) | NirExpr::NewEnum(ty, _, _) => pool.canonical_type(*ty),
                NirExpr::NewList(_) => pool.list_type(),
                _ => None,
            };
            if let Some(ty) = inferred
                && pool.as_intrinsic(ty) != Some(Intrinsic::Any)
            {
                out.insert(*dst, ty);
            }
        }
    }
    out
}

fn collect_single_def_constants(func: &NirFunction) -> HashMap<NirLocal, NirValue> {
    let mut counts: HashMap<NirLocal, usize> = HashMap::new();
    for p in &func.params {
        *counts.entry(p.local).or_default() += 1;
    }
    for block in &func.blocks {
        for stmt in &block.stmts {
            if let NirStmt::Assign(dst, _) = stmt.unscoped() {
                *counts.entry(*dst).or_default() += 1;
            }
        }
    }
    let mut out = HashMap::new();
    for block in &func.blocks {
        for stmt in &block.stmts {
            if let NirStmt::Assign(dst, expr) = stmt.unscoped()
                && counts.get(dst).copied() == Some(1)
                && let NirExpr::Use(v) = expr.unscoped()
                && !matches!(v, NirValue::Local(_))
            {
                out.insert(*dst, *v);
            }
        }
    }
    out
}
