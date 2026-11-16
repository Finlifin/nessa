//! Dominator-tree Global Value Numbering (GVN / CSE) and dominated branch
//! condition refinement.

use std::collections::{HashMap, HashSet};

use type_pool::{Intrinsic, TypeIndex, TypePool};

use crate::opt::cfg::CfgInfo;
use crate::opt::purity::is_dominator_gvn_eligible;
use crate::opt::{
    for_each_expr_read_local, for_each_stmt_value_mut, for_each_terminator_value_mut,
    split_scoped_expr, wrap_expr_preserving_scope,
};
use crate::{
    BinOp, BlockId, NirExpr, NirFunction, NirLocal, NirStmt, NirValue, Terminator, UnaryOp,
};

#[derive(Clone, PartialEq, Eq, Hash)]
enum ValKey {
    Local(u32),
    Int(i64),
    UInt(u64),
    I128(i128),
    U128(u128),
    FloatBits(u64),
    Bool(bool),
    Char(char),
    Str(u32),
    Type(u32),
    Enum(u32, u32),
    Unit,
    Null,
}

impl ValKey {
    fn from_val(v: NirValue) -> Self {
        match v {
            NirValue::Local(l) => Self::Local(l.0),
            NirValue::ConstInt(x) => Self::Int(x),
            NirValue::ConstUInt(x) => Self::UInt(x),
            NirValue::ConstI128(x) => Self::I128(x),
            NirValue::ConstU128(x) => Self::U128(x),
            NirValue::ConstFloat(x) => Self::FloatBits(x.to_bits()),
            NirValue::ConstBool(b) => Self::Bool(b),
            NirValue::ConstChar(c) => Self::Char(c),
            NirValue::ConstStr(s) => Self::Str(s.as_u32()),
            NirValue::ConstType(t) => Self::Type(t.as_u32()),
            NirValue::ConstEnum(t, tag) => Self::Enum(t.as_u32(), tag),
            NirValue::Unit => Self::Unit,
            NirValue::Null => Self::Null,
        }
    }

    fn sort_rank(&self) -> (u8, u128) {
        match *self {
            Self::Local(x) => (0, x as u128),
            Self::Int(x) => (1, x as u64 as u128),
            Self::UInt(x) => (2, x as u128),
            Self::I128(x) => (3, x as u128),
            Self::U128(x) => (4, x),
            Self::FloatBits(x) => (5, x as u128),
            Self::Bool(b) => (6, b as u128),
            Self::Char(c) => (7, c as u128),
            Self::Str(s) => (8, s as u128),
            Self::Type(t) => (9, t as u128),
            Self::Enum(t, v) => (10, ((t as u128) << 32) | v as u128),
            Self::Unit => (11, 0),
            Self::Null => (12, 0),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum GvnKey {
    BinOp(u8, ValKey, ValKey),
    UnaryOp(u8, ValKey),
    EnumIs(ValKey, u32, u32),
    EnumField(ValKey, u32),
    ErrorIsOk(ValKey),
    ErrorPayload(ValKey),
    TypeCheck(ValKey, u32, Option<u32>),
    TypeCast(ValKey, u32),
    TypeAssert(ValKey, u32, Option<u32>),
    TraitProof(ValKey, u32, Option<u32>),
    TraitProject(ValKey, u32),
    Builtin(u32, Vec<ValKey>),
}

fn build_gvn_key(expr: &NirExpr, pool: &TypePool) -> Option<GvnKey> {
    let scope = match expr {
        NirExpr::ScopedCall { scope, .. } => Some(*scope),
        _ => None,
    };
    match expr.unscoped() {
        NirExpr::BinOp(op, a, b) => {
            let mut ka = ValKey::from_val(*a);
            let mut kb = ValKey::from_val(*b);
            if is_commutative_binop(*op) && ka.sort_rank() > kb.sort_rank() {
                std::mem::swap(&mut ka, &mut kb);
            }
            Some(GvnKey::BinOp(binop_tag(*op), ka, kb))
        }
        NirExpr::UnaryOp(op, v) => {
            let tag = match op {
                UnaryOp::Neg => 0,
                UnaryOp::Not => 1,
                UnaryOp::BitNot => 2,
            };
            Some(GvnKey::UnaryOp(tag, ValKey::from_val(*v)))
        }
        NirExpr::EnumIs(v, ty, tag) => {
            let ct = pool.canonical_type(*ty)?.as_u32();
            Some(GvnKey::EnumIs(ValKey::from_val(*v), ct, *tag))
        }
        NirExpr::EnumField(v, idx) => Some(GvnKey::EnumField(ValKey::from_val(*v), *idx)),
        NirExpr::ErrorIsOk(v) => Some(GvnKey::ErrorIsOk(ValKey::from_val(*v))),
        NirExpr::ErrorPayload(v) => Some(GvnKey::ErrorPayload(ValKey::from_val(*v))),
        NirExpr::TypeCheck(v, ty) => {
            let ct = pool.canonical_type(*ty)?.as_u32();
            Some(GvnKey::TypeCheck(ValKey::from_val(*v), ct, scope))
        }
        NirExpr::TypeCast(v, ty) => {
            let ct = pool.canonical_type(*ty)?.as_u32();
            Some(GvnKey::TypeCast(ValKey::from_val(*v), ct))
        }
        NirExpr::TypeAssert(v, ty) => {
            let ct = pool.canonical_type(*ty)?.as_u32();
            Some(GvnKey::TypeAssert(ValKey::from_val(*v), ct, scope))
        }
        NirExpr::TraitProof(v, ty) => {
            let ct = pool.canonical_type(*ty)?.as_u32();
            Some(GvnKey::TraitProof(ValKey::from_val(*v), ct, scope))
        }
        NirExpr::TraitProject(v, ty) => {
            let ct = pool.canonical_type(*ty)?.as_u32();
            Some(GvnKey::TraitProject(ValKey::from_val(*v), ct))
        }
        NirExpr::CallBuiltin(id, args) => Some(GvnKey::Builtin(
            *id,
            args.iter().copied().map(ValKey::from_val).collect(),
        )),
        _ => None,
    }
}

fn is_commutative_binop(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Mul
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Eq
            | BinOp::Ne
            | BinOp::And
            | BinOp::Or
    )
}

fn binop_tag(op: BinOp) -> u8 {
    match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::Mod => 4,
        BinOp::BitAnd => 5,
        BinOp::BitOr => 6,
        BinOp::BitXor => 7,
        BinOp::Shl => 8,
        BinOp::Shr => 9,
        BinOp::Eq => 10,
        BinOp::Ne => 11,
        BinOp::Lt => 12,
        BinOp::Le => 13,
        BinOp::Gt => 14,
        BinOp::Ge => 15,
        BinOp::And => 16,
        BinOp::Or => 17,
    }
}

#[derive(Clone)]
enum CondDef {
    EnumIs(NirLocal, TypeIndex, u32),
    ErrorIsOk(NirLocal),
    TypeCheck(NirLocal, TypeIndex),
    EqConst(NirLocal, NirValue, bool),
}

struct GvnWalker<'a> {
    pool: &'a TypePool,
    cfg: &'a CfgInfo,
    dom_children: Vec<Vec<BlockId>>,
    single_def: HashSet<NirLocal>,
    cond_defs: HashMap<NirLocal, CondDef>,
    expr_table: HashMap<GvnKey, NirValue>,
    local_replacements: HashMap<NirLocal, NirValue>,
    proven_types: HashMap<NirLocal, TypeIndex>,
}

pub(crate) fn run_dominator_gvn(func: &mut NirFunction, type_pool: &TypePool) {
    if func.blocks.is_empty() {
        return;
    }
    let cfg = CfgInfo::compute(func);
    let dom_children = cfg.dom_tree_children();

    // Identify SSA-like locals assigned at most once across the entire function.
    let mut assign_counts: HashMap<NirLocal, usize> = HashMap::new();
    for p in &func.params {
        *assign_counts.entry(p.local).or_default() += 1;
    }
    for block in &func.blocks {
        for stmt in &block.stmts {
            if let NirStmt::Assign(dst, _) = stmt.unscoped() {
                *assign_counts.entry(*dst).or_default() += 1;
            }
        }
    }
    let single_def: HashSet<NirLocal> = assign_counts
        .into_iter()
        .filter_map(|(loc, count)| (count <= 1).then_some(loc))
        .collect();

    // Collect single-def boolean condition definitions for dominated branch refinement.
    let mut cond_defs = HashMap::new();
    for block in &func.blocks {
        for stmt in &block.stmts {
            let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
                continue;
            };
            if !single_def.contains(dst) {
                continue;
            }
            match expr.unscoped() {
                NirExpr::EnumIs(NirValue::Local(x), ty, tag) if single_def.contains(x) => {
                    if let Some(ct) = type_pool.canonical_type(*ty) {
                        cond_defs.insert(*dst, CondDef::EnumIs(*x, ct, *tag));
                    }
                }
                NirExpr::ErrorIsOk(NirValue::Local(x)) if single_def.contains(x) => {
                    cond_defs.insert(*dst, CondDef::ErrorIsOk(*x));
                }
                NirExpr::TypeCheck(NirValue::Local(x), ty) if single_def.contains(x) => {
                    if let Some(ct) = type_pool.canonical_type(*ty) {
                        cond_defs.insert(*dst, CondDef::TypeCheck(*x, ct));
                    }
                }
                NirExpr::BinOp(op @ (BinOp::Eq | BinOp::Ne), a, b) => {
                    let is_eq = *op == BinOp::Eq;
                    if let (NirValue::Local(x), c) | (c, NirValue::Local(x)) = (*a, *b)
                        && !matches!(c, NirValue::Local(_))
                        && single_def.contains(&x)
                        && is_exact_equality_const(c)
                    {
                        cond_defs.insert(*dst, CondDef::EqConst(x, c, is_eq));
                    }
                }
                _ => {}
            }
        }
    }

    let mut proven_types = HashMap::new();
    for p in &func.params {
        if single_def.contains(&p.local)
            && p.type_index != TypeIndex::INVALID
            && type_pool.as_intrinsic(p.type_index) != Some(Intrinsic::Any)
            && let Some(ct) = type_pool.canonical_type(p.type_index)
        {
            proven_types.insert(p.local, ct);
        }
    }

    let mut walker = GvnWalker {
        pool: type_pool,
        cfg: &cfg,
        dom_children,
        single_def,
        cond_defs,
        expr_table: HashMap::new(),
        local_replacements: HashMap::new(),
        proven_types,
    };
    walker.visit_block(func, BlockId(0));
}

/// Constants whose `==` implies bit-identical runtime representation (excludes numeric `0 == 0.0`).
fn is_exact_equality_const(c: NirValue) -> bool {
    matches!(
        c,
        NirValue::Null
            | NirValue::Unit
            | NirValue::ConstBool(_)
            | NirValue::ConstChar(_)
            | NirValue::ConstEnum(_, _)
    )
}

impl GvnWalker<'_> {
    fn canonicalize_value(&self, v: NirValue) -> NirValue {
        match v {
            NirValue::Local(loc) => self.local_replacements.get(&loc).copied().unwrap_or(v),
            other => other,
        }
    }

    fn visit_block(&mut self, func: &mut NirFunction, bid: BlockId) {
        let b_idx = bid.0 as usize;
        if b_idx >= func.blocks.len() {
            return;
        }

        let mut undo_exprs: Vec<(GvnKey, Option<NirValue>)> = Vec::new();
        let mut undo_locals: Vec<(NirLocal, Option<NirValue>)> = Vec::new();
        let mut undo_types: Vec<(NirLocal, Option<TypeIndex>)> = Vec::new();

        for stmt in &mut func.blocks[b_idx].stmts {
            for_each_stmt_value_mut(stmt, |val| {
                *val = self.canonicalize_value(*val);
            });

            let NirStmt::Assign(dst, expr) = stmt.unscoped_mut() else {
                continue;
            };
            let dst = *dst;
            if !self.single_def.contains(&dst) {
                continue;
            }

            // Dominated redundant TypeAssert / TypeCheck elimination on proven non-Trait types.
            let assert_target = match expr.unscoped() {
                NirExpr::TypeAssert(NirValue::Local(x), ty) => Some((*x, *ty)),
                _ => None,
            };
            if let Some((x, ty)) = assert_target
                && let Some(canon_ty) = self.pool.canonical_type(ty)
                && !matches!(
                    self.pool.get(canon_ty).kind,
                    type_pool::TypeKind::Trait { .. }
                )
            {
                if self.proven_types.get(&x).copied() == Some(canon_ty) {
                    let (_, scope) =
                        split_scoped_expr(std::mem::replace(expr, NirExpr::Use(NirValue::Unit)));
                    *expr = wrap_expr_preserving_scope(NirExpr::Use(NirValue::Local(x)), scope);
                    let prev = self.local_replacements.insert(dst, NirValue::Local(x));
                    undo_locals.push((dst, prev));
                    continue;
                }
                if self.single_def.contains(&x) {
                    let prev = self.proven_types.insert(x, canon_ty);
                    undo_types.push((x, prev));
                }
            }

            if let NirExpr::Use(src) = expr.unscoped() {
                let canon_src = self.canonicalize_value(*src);
                let prev = self.local_replacements.insert(dst, canon_src);
                undo_locals.push((dst, prev));
                continue;
            }

            if !is_dominator_gvn_eligible(expr) {
                continue;
            }
            let mut all_operands_single_def = true;
            for_each_expr_read_local(expr, |loc| {
                if !self.single_def.contains(&loc) {
                    all_operands_single_def = false;
                }
            });
            if !all_operands_single_def {
                continue;
            }

            let Some(key) = build_gvn_key(expr, self.pool) else {
                continue;
            };
            if let Some(&existing_val) = self.expr_table.get(&key) {
                let (_, scope) =
                    split_scoped_expr(std::mem::replace(expr, NirExpr::Use(NirValue::Unit)));
                *expr = wrap_expr_preserving_scope(NirExpr::Use(existing_val), scope);
                let prev = self.local_replacements.insert(dst, existing_val);
                undo_locals.push((dst, prev));
            } else {
                let val = NirValue::Local(dst);
                let prev = self.expr_table.insert(key.clone(), val);
                undo_exprs.push((key, prev));
            }
        }

        for_each_terminator_value_mut(&mut func.blocks[b_idx].terminator, |val| {
            *val = self.canonicalize_value(*val);
        });

        let term_snapshot = func.blocks[b_idx].terminator.clone();
        let children = self.dom_children[b_idx].clone();
        for child in children {
            let mut branch_undo_exprs = Vec::new();
            let mut branch_undo_locals = Vec::new();
            let mut branch_undo_types = Vec::new();

            if let Terminator::Branch(NirValue::Local(cond_loc), then_bb, else_bb) = term_snapshot
                && then_bb != else_bb
                && self.cfg.preds[child.0 as usize].len() == 1
                && self.cfg.preds[child.0 as usize][0] == bid
                && (child == then_bb || child == else_bb)
            {
                let taken_bool = child == then_bb;
                self.push_branch_facts(
                    cond_loc,
                    taken_bool,
                    &mut branch_undo_exprs,
                    &mut branch_undo_locals,
                    &mut branch_undo_types,
                );
            }

            self.visit_block(func, child);

            for (loc, prev) in branch_undo_types.into_iter().rev() {
                match prev {
                    Some(old) => {
                        self.proven_types.insert(loc, old);
                    }
                    None => {
                        self.proven_types.remove(&loc);
                    }
                }
            }
            for (loc, prev) in branch_undo_locals.into_iter().rev() {
                match prev {
                    Some(old) => {
                        self.local_replacements.insert(loc, old);
                    }
                    None => {
                        self.local_replacements.remove(&loc);
                    }
                }
            }
            for (k, prev) in branch_undo_exprs.into_iter().rev() {
                match prev {
                    Some(old) => {
                        self.expr_table.insert(k, old);
                    }
                    None => {
                        self.expr_table.remove(&k);
                    }
                }
            }
        }

        for (loc, prev) in undo_types.into_iter().rev() {
            match prev {
                Some(old) => {
                    self.proven_types.insert(loc, old);
                }
                None => {
                    self.proven_types.remove(&loc);
                }
            }
        }
        for (loc, prev) in undo_locals.into_iter().rev() {
            match prev {
                Some(old) => {
                    self.local_replacements.insert(loc, old);
                }
                None => {
                    self.local_replacements.remove(&loc);
                }
            }
        }
        for (k, prev) in undo_exprs.into_iter().rev() {
            match prev {
                Some(old) => {
                    self.expr_table.insert(k, old);
                }
                None => {
                    self.expr_table.remove(&k);
                }
            }
        }
    }

    fn push_branch_facts(
        &mut self,
        cond_loc: NirLocal,
        taken_bool: bool,
        undo_exprs: &mut Vec<(GvnKey, Option<NirValue>)>,
        undo_locals: &mut Vec<(NirLocal, Option<NirValue>)>,
        undo_types: &mut Vec<(NirLocal, Option<TypeIndex>)>,
    ) {
        let Some(def) = self.cond_defs.get(&cond_loc).cloned() else {
            return;
        };
        match def {
            CondDef::EnumIs(x, ty, tag) => {
                let key = GvnKey::EnumIs(ValKey::Local(x.0), ty.as_u32(), tag);
                let prev = self
                    .expr_table
                    .insert(key.clone(), NirValue::ConstBool(taken_bool));
                undo_exprs.push((key, prev));
                if taken_bool
                    && let type_pool::TypeKind::Enum { variants, .. } = &self.pool.get(ty).kind
                {
                    for v in variants {
                        if v.tag != tag {
                            let okey = GvnKey::EnumIs(ValKey::Local(x.0), ty.as_u32(), v.tag);
                            let oprev = self
                                .expr_table
                                .insert(okey.clone(), NirValue::ConstBool(false));
                            undo_exprs.push((okey, oprev));
                        }
                    }
                }
            }
            CondDef::ErrorIsOk(x) => {
                let key = GvnKey::ErrorIsOk(ValKey::Local(x.0));
                let prev = self
                    .expr_table
                    .insert(key.clone(), NirValue::ConstBool(taken_bool));
                undo_exprs.push((key, prev));
            }
            CondDef::TypeCheck(x, ty) => {
                if taken_bool
                    && !matches!(self.pool.get(ty).kind, type_pool::TypeKind::Trait { .. })
                {
                    let prev = self.proven_types.insert(x, ty);
                    undo_types.push((x, prev));
                }
            }
            CondDef::EqConst(x, c, is_eq) => {
                let holds_equal = taken_bool == is_eq;
                if holds_equal {
                    let prev = self.local_replacements.insert(x, c);
                    undo_locals.push((x, prev));
                } else {
                    let mut ka = ValKey::Local(x.0);
                    let mut kb = ValKey::from_val(c);
                    if ka.sort_rank() > kb.sort_rank() {
                        std::mem::swap(&mut ka, &mut kb);
                    }
                    let eq_key = GvnKey::BinOp(binop_tag(BinOp::Eq), ka.clone(), kb.clone());
                    let ne_key = GvnKey::BinOp(binop_tag(BinOp::Ne), ka, kb);
                    let p1 = self
                        .expr_table
                        .insert(eq_key.clone(), NirValue::ConstBool(false));
                    let p2 = self
                        .expr_table
                        .insert(ne_key.clone(), NirValue::ConstBool(true));
                    undo_exprs.push((eq_key, p1));
                    undo_exprs.push((ne_key, p2));
                }
            }
        }
    }
}
