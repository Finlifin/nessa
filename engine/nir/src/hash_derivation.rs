//! Compositional `Hash` bodies mix checked field hashes in declaration order.

use std::collections::HashMap;

use nsbc::FuncId;
use resolution::{DerivedHashPlan, ResolvedAst, SymbolId};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::builder::FunctionBuilder;
use crate::{
    BinOp, BlockId, NirExpr, NirFunction, NirParam, NirParamRole, NirStmt, NirValue, Terminator,
};

const STRUCT_SEED: u64 = 0x9e3779b97f4a7c15;
const TUPLE_SEED: u64 = 0x517cc1b727220a95;
const NEWTYPE_SEED: u64 = 0x94d049bb133111eb;
const ENUM_TAG_STEP: u64 = 0x517cc1b727220a95;
const NULL_HASH: u64 = 0xd6e8feb86659fd93;
const SOME_SEED: u64 = 0x94d049bb133111eb;
const MIX_MUL: u64 = 0xbf58476d1ce4e5b9;

pub(crate) fn lower(
    resolved: &ResolvedAst,
    plan: &DerivedHashPlan,
    function: FuncId,
    functions: &HashMap<SymbolId, FuncId>,
    derived: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
) -> NirFunction {
    let mut builder = FunctionBuilder::new(function, plan.method_name);
    builder.function_type = plan.signature;
    builder.return_type = Intrinsic::U64.type_index();
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|scope| scope.0);
    builder.func_map = functions.clone();
    builder.derived_functions = derived.clone();
    let receiver = builder.alloc_local();
    builder.params.push(NirParam {
        local: receiver,
        name: str_interner::intern("self"),
        type_index: plan.implementor,
        role: NirParamRole::User,
    });
    let mut block = builder.new_block();
    builder.entry_block = block;
    let mut emitter = HashEmitter {
        resolved,
        builder: &mut builder,
    };
    let result = emitter.aggregate(plan.implementor, NirValue::Local(receiver), &mut block);
    let result = emitter.value(
        block,
        NirExpr::TypeAssert(result, Intrinsic::U64.type_index()),
    );
    emitter.builder.blocks[block.0 as usize].terminator = Terminator::Return(result);
    builder.build()
}

struct HashEmitter<'a> {
    resolved: &'a ResolvedAst,
    builder: &'a mut FunctionBuilder,
}

impl HashEmitter<'_> {
    fn value(&mut self, block: BlockId, expression: NirExpr) -> NirValue {
        let expression = if expression.requires_scope() {
            expression.in_scope(
                self.builder
                    .entry_scope
                    .expect("checked derive declaration scope"),
            )
        } else {
            expression
        };
        let local = self.builder.alloc_local();
        self.builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, expression));
        NirValue::Local(local)
    }

    fn mix(&mut self, block: BlockId, acc: NirValue, item: NirValue) -> NirValue {
        let xored = self.value(block, NirExpr::BinOp(BinOp::BitXor, acc, item));
        let mixed = self.value(
            block,
            NirExpr::CallBuiltin(
                runtime::ids::WRAPPING_MUL,
                vec![xored, NirValue::ConstUInt(MIX_MUL)],
            ),
        );
        self.value(
            block,
            NirExpr::TypeAssert(mixed, Intrinsic::U64.type_index()),
        )
    }

    fn function(&self, ty: TypeIndex) -> Option<FuncId> {
        let trait_type = self.resolved.type_pool.well_known.hash;
        let method = self.resolved.type_pool.find_trait_method(
            ty,
            trait_type,
            str_interner::intern("hash"),
        )?;
        if method.func_id == type_pool::DERIVE_FUNC_ID {
            self.builder
                .derived_functions
                .get(&(ty, trait_type, method.name))
                .copied()
        } else {
            self.builder
                .func_map
                .get(&SymbolId(method.func_id))
                .copied()
        }
    }

    fn component(&mut self, ty: TypeIndex, value: NirValue, block: &mut BlockId) -> NirValue {
        let ty = self
            .resolved
            .type_pool
            .canonical_type(ty)
            .expect("checked Hash field type");
        if let Some(function) = self.function(ty) {
            let hashed = self.value(*block, NirExpr::Call(function, vec![value]));
            return self.value(
                *block,
                NirExpr::TypeAssert(hashed, Intrinsic::U64.type_index()),
            );
        }
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Optional { inner } => {
                if self.resolved.type_pool.as_intrinsic(inner) == Some(Intrinsic::NoReturn) {
                    return NirValue::ConstUInt(NULL_HASH);
                }
                let is_null = self.value(*block, NirExpr::BinOp(BinOp::Eq, value, NirValue::Null));
                let null = self.builder.new_block();
                let mut nonnull = self.builder.new_block();
                let join = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_null, null, nonnull);
                let result = self.builder.alloc_local();
                self.builder.blocks[null.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(
                        result,
                        NirExpr::Use(NirValue::ConstUInt(NULL_HASH)),
                    ));
                self.builder.blocks[null.0 as usize].terminator = Terminator::Goto(join);
                let inner_hash = self.component(inner, value, &mut nonnull);
                let combined = self.mix(nonnull, NirValue::ConstUInt(SOME_SEED), inner_hash);
                self.builder.blocks[nonnull.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(result, NirExpr::Use(combined)));
                self.builder.blocks[nonnull.0 as usize].terminator = Terminator::Goto(join);
                *block = join;
                NirValue::Local(result)
            }
            TypeKind::Tuple { elements } => self.fields(
                &elements,
                value,
                false,
                NirValue::ConstUInt(TUPLE_SEED),
                block,
            ),
            _ => unreachable!("Hash field prerequisite was checked before lowering"),
        }
    }

    fn fields(
        &mut self,
        types: &[TypeIndex],
        receiver: NirValue,
        enum_fields: bool,
        mut acc: NirValue,
        block: &mut BlockId,
    ) -> NirValue {
        for (index, &ty) in types.iter().enumerate() {
            let expression = if enum_fields {
                NirExpr::EnumField(receiver, index as u32)
            } else {
                NirExpr::FieldAccess(receiver, index as u32)
            };
            let field_val = self.value(*block, expression);
            let field_hash = self.component(ty, field_val, block);
            acc = self.mix(*block, acc, field_hash);
        }
        acc
    }

    fn aggregate(&mut self, ty: TypeIndex, receiver: NirValue, block: &mut BlockId) -> NirValue {
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Struct { fields, .. } => {
                let types: Vec<_> = fields.iter().map(|field| field.ty).collect();
                self.fields(
                    &types,
                    receiver,
                    false,
                    NirValue::ConstUInt(STRUCT_SEED),
                    block,
                )
            }
            TypeKind::Tuple { elements } => self.fields(
                &elements,
                receiver,
                false,
                NirValue::ConstUInt(TUPLE_SEED),
                block,
            ),
            TypeKind::Newtype { inner, .. } => self.fields(
                &[inner],
                receiver,
                false,
                NirValue::ConstUInt(NEWTYPE_SEED),
                block,
            ),
            TypeKind::Enum { variants, .. } => {
                let result = self.builder.alloc_local();
                let join = self.builder.new_block();
                for variant in &variants {
                    let condition = self.value(*block, NirExpr::EnumIs(receiver, ty, variant.tag));
                    let mut selected = self.builder.new_block();
                    let next = self.builder.new_block();
                    self.builder.blocks[block.0 as usize].terminator =
                        Terminator::Branch(condition, selected, next);
                    let variant_seed =
                        STRUCT_SEED.wrapping_add((variant.tag as u64).wrapping_mul(ENUM_TAG_STEP));
                    let types: Vec<_> = variant.fields.iter().map(|field| field.ty).collect();
                    let hashed = self.fields(
                        &types,
                        receiver,
                        true,
                        NirValue::ConstUInt(variant_seed),
                        &mut selected,
                    );
                    self.builder.blocks[selected.0 as usize]
                        .stmts
                        .push(NirStmt::Assign(result, NirExpr::Use(hashed)));
                    self.builder.blocks[selected.0 as usize].terminator = Terminator::Goto(join);
                    *block = next;
                }
                self.builder.blocks[block.0 as usize].terminator = Terminator::MatchFail;
                *block = join;
                NirValue::Local(result)
            }
            _ => unreachable!("Hash derivation target is an aggregate"),
        }
    }
}
