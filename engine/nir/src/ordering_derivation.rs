//! Lexicographic derived ordering uses checked field functions and ordinary VM frames.

use std::collections::HashMap;

use nsbc::FuncId;
use resolution::{DerivedComparisonPlan, ResolvedAst, SymbolId};
use str_interner::StrId;
use type_pool::{TypeIndex, TypeKind};

use crate::builder::FunctionBuilder;
use crate::ordering::OrderingLayout;
use crate::{BinOp, BlockId, NirExpr, NirFunction, NirParam, NirStmt, NirValue, Terminator};

pub(crate) fn lower(
    resolved: &ResolvedAst,
    plan: &DerivedComparisonPlan,
    function: FuncId,
    functions: &HashMap<SymbolId, FuncId>,
    derived: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
) -> NirFunction {
    let TypeKind::Function { ret, .. } = resolved.type_pool.get(plan.signature).kind else {
        unreachable!("checked ordering signature");
    };
    let ordering = OrderingLayout::from_result(&resolved.type_pool, ret)
        .expect("checked nominal Ordering result");
    let mut builder = FunctionBuilder::new(function, plan.method_name);
    builder.function_type = plan.signature;
    builder.return_type = ret;
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|scope| scope.0);
    builder.func_map = functions.clone();
    builder.derived_functions = derived.clone();
    let left = builder.alloc_local();
    let right = builder.alloc_local();
    for (local, name) in [(left, "self"), (right, "other")] {
        builder.params.push(NirParam {
            local,
            name: str_interner::intern(name),
            type_index: plan.implementor,
            role: crate::NirParamRole::User,
        });
    }
    let entry = builder.new_block();
    builder.entry_block = entry;
    let equal = builder.new_block();
    let done = builder.new_block();
    let result = builder.alloc_local();
    builder.blocks[equal.0 as usize].terminator =
        Terminator::Return(NirValue::ConstEnum(ordering.ty, ordering.equal));
    builder.blocks[done.0 as usize].terminator = Terminator::Return(NirValue::Local(result));
    let mut emitter = OrderingEmitter {
        resolved,
        builder: &mut builder,
        ordering,
        trait_type: plan.trait_type,
        done,
        result,
    };
    emitter.aggregate(
        plan.implementor,
        NirValue::Local(left),
        NirValue::Local(right),
        entry,
        equal,
    );
    builder.build()
}

struct OrderingEmitter<'a> {
    resolved: &'a ResolvedAst,
    builder: &'a mut FunctionBuilder,
    ordering: OrderingLayout,
    trait_type: TypeIndex,
    done: BlockId,
    result: crate::NirLocal,
}

impl OrderingEmitter<'_> {
    fn value(&mut self, block: BlockId, expression: NirExpr) -> NirValue {
        let expression = if expression.requires_scope() {
            expression.in_scope(self.builder.entry_scope.expect("derive declaration scope"))
        } else {
            expression
        };
        let local = self.builder.alloc_local();
        self.builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, expression));
        NirValue::Local(local)
    }
    fn branch(&mut self, block: BlockId, condition: NirValue, yes: BlockId, no: BlockId) {
        self.builder.blocks[block.0 as usize].terminator = Terminator::Branch(condition, yes, no);
    }
    fn finish(&mut self, block: BlockId, value: NirValue) {
        self.builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(self.result, NirExpr::Use(value)));
        self.builder.blocks[block.0 as usize].terminator = Terminator::Goto(self.done);
    }
    fn constant(&mut self, block: BlockId, tag: u32) {
        self.finish(block, NirValue::ConstEnum(self.ordering.ty, tag));
    }
    fn fields(
        &mut self,
        fields: &[TypeIndex],
        left: NirValue,
        right: NirValue,
        enum_fields: bool,
        mut block: BlockId,
        equal: BlockId,
    ) {
        for (index, &ty) in fields.iter().enumerate() {
            let field = |value| {
                if enum_fields {
                    NirExpr::EnumField(value, index as u32)
                } else {
                    NirExpr::FieldAccess(value, index as u32)
                }
            };
            let left = self.value(block, field(left));
            let right = self.value(block, field(right));
            let next = self.builder.new_block();
            self.compare(ty, left, right, block, next);
            block = next;
        }
        self.builder.blocks[block.0 as usize].terminator = Terminator::Goto(equal);
    }
    fn aggregate(
        &mut self,
        ty: TypeIndex,
        left: NirValue,
        right: NirValue,
        mut block: BlockId,
        equal: BlockId,
    ) {
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Struct { fields, .. } => self.fields(
                &fields.iter().map(|field| field.ty).collect::<Vec<_>>(),
                left,
                right,
                false,
                block,
                equal,
            ),
            TypeKind::Tuple { elements } => {
                self.fields(&elements, left, right, false, block, equal)
            }
            TypeKind::Enum { variants, .. } => {
                for (left_index, variant) in variants.iter().enumerate() {
                    let matched = self.builder.new_block();
                    let next = self.builder.new_block();
                    let condition = self.value(block, NirExpr::EnumIs(left, ty, variant.tag));
                    self.branch(block, condition, matched, next);
                    let mut right_block = matched;
                    for (right_index, right_variant) in variants.iter().enumerate() {
                        let payload = self.builder.new_block();
                        let next_right = self.builder.new_block();
                        let condition =
                            self.value(right_block, NirExpr::EnumIs(right, ty, right_variant.tag));
                        self.branch(right_block, condition, payload, next_right);
                        if left_index == right_index {
                            self.fields(
                                &variant
                                    .fields
                                    .iter()
                                    .map(|field| field.ty)
                                    .collect::<Vec<_>>(),
                                left,
                                right,
                                true,
                                payload,
                                equal,
                            );
                        } else {
                            self.constant(
                                payload,
                                if left_index < right_index {
                                    self.ordering.less
                                } else {
                                    self.ordering.greater
                                },
                            );
                        }
                        right_block = next_right;
                    }
                    self.builder.blocks[right_block.0 as usize].terminator = Terminator::MatchFail;
                    block = next;
                }
                self.builder.blocks[block.0 as usize].terminator = Terminator::MatchFail;
            }
            _ => unreachable!("ordering derivation target is an aggregate"),
        }
    }
    fn function(&self, ty: TypeIndex) -> Option<(FuncId, TypeIndex)> {
        let find = |trait_type, name| {
            let method = self.resolved.type_pool.find_trait_method(
                ty,
                trait_type,
                str_interner::intern(name),
            )?;
            if method.func_id == type_pool::DERIVE_FUNC_ID {
                let function = self
                    .builder
                    .derived_functions
                    .get(&(ty, trait_type, method.name))
                    .copied()?;
                let signature = self
                    .resolved
                    .derived_comparisons
                    .iter()
                    .find(|plan| plan.implementor == ty && plan.trait_type == trait_type)?
                    .signature;
                let TypeKind::Function { ret, .. } = self.resolved.type_pool.get(signature).kind
                else {
                    return None;
                };
                Some((function, ret))
            } else {
                let symbol = SymbolId(method.func_id);
                let function = self.builder.func_map.get(&symbol).copied()?;
                let signature = self
                    .resolved
                    .type_pool
                    .canonical_type(self.resolved.symbols[symbol.0 as usize].type_index)?;
                let TypeKind::Function { ret, .. } = self.resolved.type_pool.get(signature).kind
                else {
                    return None;
                };
                Some((function, ret))
            }
        };
        if self.trait_type == self.resolved.type_pool.well_known.ord {
            find(self.trait_type, "cmp")
        } else {
            find(self.trait_type, "partial_cmp")
                .or_else(|| find(self.resolved.type_pool.well_known.ord, "cmp"))
        }
    }
    fn compare(
        &mut self,
        ty: TypeIndex,
        left: NirValue,
        right: NirValue,
        block: BlockId,
        equal: BlockId,
    ) {
        let ty = self
            .resolved
            .type_pool
            .canonical_type(ty)
            .expect("checked ordering component");
        if let TypeKind::Optional { inner } = self.resolved.type_pool.get(ty).kind {
            let null_left = self.builder.new_block();
            let nonnull_left = self.builder.new_block();
            let condition = self.value(block, NirExpr::BinOp(BinOp::Eq, left, NirValue::Null));
            self.branch(block, condition, null_left, nonnull_left);
            let less = self.builder.new_block();
            let condition = self.value(null_left, NirExpr::BinOp(BinOp::Eq, right, NirValue::Null));
            self.branch(null_left, condition, equal, less);
            self.constant(less, self.ordering.less);
            let greater = self.builder.new_block();
            let values = self.builder.new_block();
            let condition = self.value(
                nonnull_left,
                NirExpr::BinOp(BinOp::Eq, right, NirValue::Null),
            );
            self.branch(nonnull_left, condition, greater, values);
            self.constant(greater, self.ordering.greater);
            self.compare(inner, left, right, values, equal);
        } else if let Some((function, result_type)) = self.function(ty) {
            let value = self.value(block, NirExpr::Call(function, vec![left, right]));
            let value = self.value(block, NirExpr::TypeAssert(value, result_type));
            let condition = self.value(
                block,
                NirExpr::EnumIs(value, self.ordering.ty, self.ordering.equal),
            );
            let unequal = self.builder.new_block();
            self.branch(block, condition, equal, unequal);
            self.finish(unequal, value);
        } else if matches!(self.resolved.type_pool.get(ty).kind, TypeKind::Tuple { .. }) {
            self.aggregate(ty, left, right, block, equal);
        } else {
            unreachable!("resolution checked the field ordering implementation");
        }
    }
}
