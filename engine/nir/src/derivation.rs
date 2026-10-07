//! Derived comparisons are ordinary functions, so field methods retain VM frames and roots.

use std::collections::HashMap;

use nsbc::FuncId;
use resolution::{DerivedComparisonPlan, ResolvedAst, SymbolId};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::builder::FunctionBuilder;
use crate::{BinOp, BlockId, NirExpr, NirFunction, NirParam, NirStmt, NirValue, Terminator};

type ComparisonFunctions = HashMap<(TypeIndex, TypeIndex, StrId), FuncId>;

pub(crate) fn comparison_function(
    resolved: &ResolvedAst,
    builder: &FunctionBuilder,
    owner: TypeIndex,
    trait_type: TypeIndex,
) -> Option<FuncId> {
    let name = str_interner::intern("eq");
    let method = resolved
        .type_pool
        .find_trait_method(owner, trait_type, name)?;
    if method.func_id == type_pool::DERIVE_FUNC_ID {
        builder
            .derived_functions
            .get(&(owner, trait_type, name))
            .copied()
    } else {
        builder.func_map.get(&SymbolId(method.func_id)).copied()
    }
}

pub(crate) fn lower(
    resolved: &ResolvedAst,
    plan: &DerivedComparisonPlan,
    function: FuncId,
    source_functions: &HashMap<SymbolId, FuncId>,
    derived_functions: &ComparisonFunctions,
) -> NirFunction {
    if plan.trait_type == resolved.type_pool.well_known.ord
        || plan.trait_type == resolved.type_pool.well_known.partial_ord
    {
        return crate::ordering_derivation::lower(
            resolved,
            plan,
            function,
            source_functions,
            derived_functions,
        );
    }
    let mut builder = FunctionBuilder::new(function, plan.method_name);
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|scope| scope.0);
    builder.function_type = plan.signature;
    builder.return_type = Intrinsic::Bool.type_index();
    builder.func_map = source_functions.clone();
    builder.derived_functions = derived_functions.clone();
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
    let yes = builder.new_block();
    let no = builder.new_block();
    builder.blocks[yes.0 as usize].terminator = Terminator::Return(NirValue::ConstBool(true));
    builder.blocks[no.0 as usize].terminator = Terminator::Return(NirValue::ConstBool(false));
    let mut emitter = ComparisonEmitter {
        resolved,
        builder: &mut builder,
        trait_type: plan.trait_type,
    };
    emitter.aggregate(
        plan.implementor,
        NirValue::Local(left),
        NirValue::Local(right),
        entry,
        yes,
        no,
    );
    builder.build()
}

struct ComparisonEmitter<'a> {
    resolved: &'a ResolvedAst,
    builder: &'a mut FunctionBuilder,
    trait_type: TypeIndex,
}

impl ComparisonEmitter<'_> {
    fn value(&mut self, block: BlockId, expression: NirExpr) -> NirValue {
        let expression = if expression.requires_scope() {
            match self.builder.entry_scope {
                Some(scope) => expression.in_scope(scope),
                None => expression,
            }
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

    fn fields(
        &mut self,
        types: &[TypeIndex],
        values: (NirValue, NirValue),
        enum_fields: bool,
        mut block: BlockId,
        yes: BlockId,
        no: BlockId,
    ) {
        for (index, &ty) in types.iter().enumerate() {
            let expression = |value| {
                if enum_fields {
                    NirExpr::EnumField(value, index as u32)
                } else {
                    NirExpr::FieldAccess(value, index as u32)
                }
            };
            let left = self.value(block, expression(values.0));
            let right = self.value(block, expression(values.1));
            let next = self.builder.new_block();
            self.compare(ty, left, right, block, next, no);
            block = next;
        }
        self.builder.blocks[block.0 as usize].terminator = Terminator::Goto(yes);
    }

    fn aggregate(
        &mut self,
        ty: TypeIndex,
        left: NirValue,
        right: NirValue,
        mut block: BlockId,
        yes: BlockId,
        no: BlockId,
    ) {
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Struct { fields, .. } => {
                self.fields(
                    &fields.iter().map(|field| field.ty).collect::<Vec<_>>(),
                    (left, right),
                    false,
                    block,
                    yes,
                    no,
                );
            }
            TypeKind::Tuple { elements } => {
                self.fields(&elements, (left, right), false, block, yes, no)
            }
            TypeKind::Enum { variants, .. } => {
                for variant in variants {
                    let matched = self.builder.new_block();
                    let next = self.builder.new_block();
                    let condition = self.value(block, NirExpr::EnumIs(left, ty, variant.tag));
                    self.branch(block, condition, matched, next);
                    let payload = self.builder.new_block();
                    let condition = self.value(matched, NirExpr::EnumIs(right, ty, variant.tag));
                    self.branch(matched, condition, payload, no);
                    self.fields(
                        &variant
                            .fields
                            .iter()
                            .map(|field| field.ty)
                            .collect::<Vec<_>>(),
                        (left, right),
                        true,
                        payload,
                        yes,
                        no,
                    );
                    block = next;
                }
                // Typed parameters exclude malformed values; retain a checked runtime failure.
                self.builder.blocks[block.0 as usize].terminator = Terminator::MatchFail;
            }
            _ => self.compare(ty, left, right, block, yes, no),
        }
    }

    fn field_function(&self, ty: TypeIndex) -> Option<FuncId> {
        comparison_function(self.resolved, self.builder, ty, self.trait_type).or_else(|| {
            (self.trait_type == self.resolved.type_pool.well_known.partial_eq)
                .then(|| {
                    comparison_function(
                        self.resolved,
                        self.builder,
                        ty,
                        self.resolved.type_pool.well_known.eq,
                    )
                })
                .flatten()
        })
    }

    fn compare(
        &mut self,
        ty: TypeIndex,
        left: NirValue,
        right: NirValue,
        block: BlockId,
        yes: BlockId,
        no: BlockId,
    ) {
        // Resolution validates every component before NIR construction.
        let ty = self
            .resolved
            .type_pool
            .canonical_type(ty)
            .expect("checked comparison component");
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Optional { inner } => {
                let left_null = self.value(block, NirExpr::BinOp(BinOp::Eq, left, NirValue::Null));
                let null_case = self.builder.new_block();
                let nonnull_case = self.builder.new_block();
                self.branch(block, left_null, null_case, nonnull_case);
                let right_null =
                    self.value(null_case, NirExpr::BinOp(BinOp::Eq, right, NirValue::Null));
                self.branch(null_case, right_null, yes, no);
                let right_null = self.value(
                    nonnull_case,
                    NirExpr::BinOp(BinOp::Eq, right, NirValue::Null),
                );
                let both_nonnull = self.builder.new_block();
                self.branch(nonnull_case, right_null, no, both_nonnull);
                self.compare(inner, left, right, both_nonnull, yes, no);
            }
            TypeKind::Tuple { elements } => {
                if let Some(function) = self.field_function(ty) {
                    let condition = self.value(block, NirExpr::Call(function, vec![left, right]));
                    self.branch(block, condition, yes, no);
                } else {
                    self.fields(&elements, (left, right), false, block, yes, no);
                }
            }
            TypeKind::Struct { .. } | TypeKind::Enum { .. } => {
                let function = self
                    .field_function(ty)
                    .expect("resolution validated field comparison implementation");
                let condition = self.value(block, NirExpr::Call(function, vec![left, right]));
                self.branch(block, condition, yes, no);
            }
            _ => {
                let condition = self.value(block, NirExpr::BinOp(BinOp::Eq, left, right));
                self.branch(block, condition, yes, no);
            }
        }
    }
}
