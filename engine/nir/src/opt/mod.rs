//! Mid-end NIR optimization pipeline (`O0`, `O1`, `O2`, `O3`).
//!
//! All passes preserve Nessa's gradual typing boundaries, runtime trap semantics
//! (`NumericOverflow`, `DivisionByZero`, `TypeError`, uninitialized globals),
//! algebraic effect handler stack discipline, and lexical `ScopedCall` / `Scoped`
//! metadata required by `codegen` and `nsbc::validate_artifact`.

pub mod cfg;
pub mod const_fold;
pub mod copy_prop;
pub mod dce;
pub mod dfe;
pub mod gvn;
pub mod inline;
pub mod licm;
pub mod purity;
pub mod sccp;
pub mod tailrec;

use type_pool::TypePool;

use crate::{BlockId, NirExpr, NirLocal, NirModule, NirStmt, NirValue, Terminator};

/// AOT compiler optimization tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum OptLevel {
    /// Unoptimized fast compilation; preserves source-level function/slot structure.
    #[default]
    O0 = 0,
    /// Local simplifications: CFG cleanup, constant/identity folding, copy propagation,
    /// dead-code/slot compaction, whole-module dead function elimination, and bytecode peepholes.
    O1 = 1,
    /// Standard AOT optimizations: Sparse Conditional Constant Propagation (SCCP),
    /// dominator-tree Global Value Numbering (GVN/CSE) + condition refinement,
    /// non-recursive small-function inlining, and post-inline cleanup.
    O2 = 2,
    /// Aggressive AOT optimizations: self-tail-recursion elimination into Tier-2-ready loops,
    /// multi-round aggressive inlining, natural-loop preheader insertion + Loop-Invariant
    /// Code Motion (LICM), and full fixed-point SCCP/GVN/DCE/DFE.
    O3 = 3,
}

impl OptLevel {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::O0),
            1 => Some(Self::O1),
            2 => Some(Self::O2),
            3 => Some(Self::O3),
            _ => None,
        }
    }

    pub fn parse_str(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "0" | "o0" => Some(Self::O0),
            "1" | "o1" => Some(Self::O1),
            "2" | "o2" | "default" => Some(Self::O2),
            "3" | "o3" => Some(Self::O3),
            _ => None,
        }
    }

    /// Resolve default optimization level from `NESSA_OPT_LEVEL` when set,
    /// otherwise `OptLevel::O0`.
    pub fn from_env() -> Self {
        std::env::var("NESSA_OPT_LEVEL")
            .ok()
            .and_then(|val| Self::parse_str(&val))
            .unwrap_or(Self::O0)
    }
}

/// Canonical value equality on `NirValue` that uses `f64::to_bits` for `ConstFloat`
/// so `-0.0` and `+0.0` and distinct `NaN` payloads are never conflated.
pub(crate) fn nir_value_bits_eq(a: &NirValue, b: &NirValue) -> bool {
    match (a, b) {
        (NirValue::Local(x), NirValue::Local(y)) => x == y,
        (NirValue::ConstInt(x), NirValue::ConstInt(y)) => x == y,
        (NirValue::ConstUInt(x), NirValue::ConstUInt(y)) => x == y,
        (NirValue::ConstI128(x), NirValue::ConstI128(y)) => x == y,
        (NirValue::ConstU128(x), NirValue::ConstU128(y)) => x == y,
        (NirValue::ConstFloat(x), NirValue::ConstFloat(y)) => x.to_bits() == y.to_bits(),
        (NirValue::ConstBool(x), NirValue::ConstBool(y)) => x == y,
        (NirValue::ConstChar(x), NirValue::ConstChar(y)) => x == y,
        (NirValue::ConstStr(x), NirValue::ConstStr(y)) => x == y,
        (NirValue::ConstType(x), NirValue::ConstType(y)) => x == y,
        (NirValue::ConstEnum(tx, vx), NirValue::ConstEnum(ty, vy)) => tx == ty && vx == vy,
        (NirValue::Unit, NirValue::Unit) | (NirValue::Null, NirValue::Null) => true,
        _ => false,
    }
}

/// Extract `(inner_expr, outer_scope)` from a `NirExpr`.
pub(crate) fn split_scoped_expr(expr: NirExpr) -> (NirExpr, Option<u32>) {
    match expr {
        NirExpr::ScopedCall { scope, call } => {
            let (inner, _) = split_scoped_expr(*call);
            (inner, Some(scope))
        }
        other => (other, None),
    }
}

/// Re-wrap `expr` in `ScopedCall` if and only if `expr.requires_scope()` is true,
/// guaranteeing the `codegen::validation` scope bijection invariant.
pub(crate) fn wrap_expr_preserving_scope(expr: NirExpr, scope: Option<u32>) -> NirExpr {
    let (inner, existing_scope) = split_scoped_expr(expr);
    let effective_scope = existing_scope.or(scope);
    if inner.requires_scope() {
        match effective_scope {
            Some(sc) => NirExpr::ScopedCall {
                scope: sc,
                call: Box::new(inner),
            },
            None => inner,
        }
    } else {
        inner
    }
}

/// Re-wrap `stmt` in `Scoped` if and only if `stmt.requires_scope()` is true.
pub(crate) fn wrap_stmt_preserving_scope(stmt: NirStmt, scope: Option<u32>) -> NirStmt {
    let (inner, existing_scope) = match stmt {
        NirStmt::Scoped {
            scope: sc,
            statement,
        } => (*statement, Some(sc)),
        other => (other, None),
    };
    let effective_scope = existing_scope.or(scope);
    if inner.requires_scope() {
        match effective_scope {
            Some(sc) => NirStmt::Scoped {
                scope: sc,
                statement: Box::new(inner),
            },
            None => inner,
        }
    } else {
        inner
    }
}

/// Visit every `NirValue` operand read inside `expr` mutably.
pub(crate) fn for_each_expr_value_mut(expr: &mut NirExpr, mut f: impl FnMut(&mut NirValue)) {
    visit_expr_values_mut(expr, &mut f);
}

fn visit_expr_values_mut(expr: &mut NirExpr, f: &mut dyn FnMut(&mut NirValue)) {
    match expr {
        NirExpr::ScopedCall { call, .. } => visit_expr_values_mut(call, f),
        NirExpr::LoadGlobal(_) | NirExpr::NewList(_) => {}
        NirExpr::Use(v)
        | NirExpr::NewEnum(_, _, v)
        | NirExpr::EnumIs(v, _, _)
        | NirExpr::EnumField(v, _)
        | NirExpr::ErrorOk(v, _)
        | NirExpr::ErrorErr(v, _)
        | NirExpr::ErrorIsOk(v)
        | NirExpr::ErrorPayload(v)
        | NirExpr::UnaryOp(_, v)
        | NirExpr::FieldAccess(v, _)
        | NirExpr::TypeCheck(v, _)
        | NirExpr::TypeCast(v, _)
        | NirExpr::TypeAssert(v, _)
        | NirExpr::TraitProof(v, _)
        | NirExpr::TraitProject(v, _)
        | NirExpr::Await(v)
        | NirExpr::DelimitedCall { body: v, .. } => f(v),
        NirExpr::TraitAssert(v, p, _) => {
            f(v);
            f(p);
        }
        NirExpr::TraitCall {
            receiver,
            proof,
            args,
            ..
        } => {
            f(receiver);
            f(proof);
            for a in args {
                f(a);
            }
        }
        NirExpr::BinOp(_, a, b)
        | NirExpr::IndexAccess(a, b)
        | NirExpr::ResumeContinuation(a, b)
        | NirExpr::ResumeContinuationOnce(a, b) => {
            f(a);
            f(b);
        }
        NirExpr::Call(_, args)
        | NirExpr::CallWithProof(_, args)
        | NirExpr::CallBuiltin(_, args)
        | NirExpr::EffectCall(_, args)
        | NirExpr::NewClosure(_, args)
        | NirExpr::NewObject(_, args) => {
            for a in args {
                f(a);
            }
        }
        NirExpr::CallIndirect(recv, args)
        | NirExpr::CallIndirectProof(recv, args)
        | NirExpr::MethodCall(recv, _, args) => {
            f(recv);
            for a in args {
                f(a);
            }
        }
    }
}

/// Visit every `NirLocal` read inside `expr`.
pub(crate) fn for_each_expr_read_local(expr: &NirExpr, mut f: impl FnMut(NirLocal)) {
    let mut cloned = expr.clone();
    for_each_expr_value_mut(&mut cloned, |v| {
        if let NirValue::Local(loc) = *v {
            f(loc);
        }
    });
}

/// Visit every `NirValue` operand read inside `stmt` mutably (does not visit `Assign` LHS).
pub(crate) fn for_each_stmt_value_mut(stmt: &mut NirStmt, mut f: impl FnMut(&mut NirValue)) {
    visit_stmt_values_mut(stmt, &mut f);
}

fn visit_stmt_values_mut(stmt: &mut NirStmt, f: &mut dyn FnMut(&mut NirValue)) {
    match stmt {
        NirStmt::Scoped { statement, .. } => visit_stmt_values_mut(statement, f),
        NirStmt::StoreIndex(obj, idx, val) => {
            f(obj);
            f(idx);
            f(val);
        }
        NirStmt::StoreField(obj, _, val) => {
            f(obj);
            f(val);
        }
        NirStmt::Assign(_, expr) => visit_expr_values_mut(expr, f),
        NirStmt::StoreGlobal(_, val) => f(val),
        NirStmt::PushHandler { closure, .. } => f(closure),
        NirStmt::PopHandler | NirStmt::AwaitPopHandler | NirStmt::Nop => {}
    }
}

/// Visit every `NirValue` operand read inside `term` mutably.
pub(crate) fn for_each_terminator_value_mut(
    term: &mut Terminator,
    mut f: impl FnMut(&mut NirValue),
) {
    match term {
        Terminator::Branch(cond, _, _) | Terminator::Return(cond) => f(cond),
        Terminator::Goto(_) | Terminator::MatchFail | Terminator::Unreachable => {}
    }
}

/// Visit every successor `BlockId` of `term`.
pub(crate) fn terminator_successors(term: &Terminator) -> Vec<BlockId> {
    match *term {
        Terminator::Goto(t) => vec![t],
        Terminator::Branch(_, t, e) => {
            if t == e {
                vec![t]
            } else {
                vec![t, e]
            }
        }
        Terminator::Return(_) | Terminator::MatchFail | Terminator::Unreachable => Vec::new(),
    }
}

/// Run the mid-end NIR optimization pipeline for `level`.
pub fn optimize_module(module: &mut NirModule, type_pool: &mut TypePool, level: OptLevel) {
    match level {
        OptLevel::O0 => {}
        OptLevel::O1 => {
            dfe::eliminate_dead_functions(module, type_pool);
            for func in &mut module.functions {
                cfg::simplify_cfg(func);
                const_fold::fold_constants_and_identities(func, type_pool);
                copy_prop::propagate_copies(func);
                cfg::simplify_cfg(func);
                dce::eliminate_dead_code_and_compact_slots(func, type_pool);
            }
            dfe::eliminate_dead_functions(module, type_pool);
        }
        OptLevel::O2 => {
            dfe::eliminate_dead_functions(module, type_pool);
            for func in &mut module.functions {
                cfg::simplify_cfg(func);
                sccp::run_sccp(func, type_pool);
                const_fold::fold_constants_and_identities(func, type_pool);
                copy_prop::propagate_copies(func);
                gvn::run_dominator_gvn(func, type_pool);
                dce::eliminate_dead_code(func, type_pool);
                cfg::simplify_cfg(func);
            }
            inline::inline_functions(module, type_pool, inline::InlineConfig::O2);
            for func in &mut module.functions {
                cfg::simplify_cfg(func);
                sccp::run_sccp(func, type_pool);
                const_fold::fold_constants_and_identities(func, type_pool);
                copy_prop::propagate_copies(func);
                gvn::run_dominator_gvn(func, type_pool);
                copy_prop::propagate_copies(func);
                const_fold::fold_constants_and_identities(func, type_pool);
                sccp::run_sccp(func, type_pool);
                cfg::simplify_cfg(func);
                dce::eliminate_dead_code_and_compact_slots(func, type_pool);
            }
            dfe::eliminate_dead_functions(module, type_pool);
        }
        OptLevel::O3 => {
            dfe::eliminate_dead_functions(module, type_pool);
            for func in &mut module.functions {
                tailrec::eliminate_self_tail_recursion(func, type_pool);
                cfg::simplify_cfg(func);
                sccp::run_sccp(func, type_pool);
                const_fold::fold_constants_and_identities(func, type_pool);
                copy_prop::propagate_copies(func);
                gvn::run_dominator_gvn(func, type_pool);
                dce::eliminate_dead_code(func, type_pool);
                cfg::simplify_cfg(func);
            }
            inline::inline_functions(module, type_pool, inline::InlineConfig::O3);
            for func in &mut module.functions {
                tailrec::eliminate_self_tail_recursion(func, type_pool);
                cfg::simplify_cfg(func);
                sccp::run_sccp(func, type_pool);
                const_fold::fold_constants_and_identities(func, type_pool);
                copy_prop::propagate_copies(func);
                gvn::run_dominator_gvn(func, type_pool);
                licm::hoist_loop_invariants(func);
                copy_prop::propagate_copies(func);
                const_fold::fold_constants_and_identities(func, type_pool);
                gvn::run_dominator_gvn(func, type_pool);
                sccp::run_sccp(func, type_pool);
                cfg::simplify_cfg(func);
                dce::eliminate_dead_code_and_compact_slots(func, type_pool);
            }
            dfe::eliminate_dead_functions(module, type_pool);
        }
    }
}
