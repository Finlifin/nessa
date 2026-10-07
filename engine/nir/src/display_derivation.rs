//! Compositional Display bodies call checked field implementations in declaration order.

use std::collections::HashMap;

use nsbc::FuncId;
use resolution::{DerivedDisplayPlan, DisplayDerivationMode, ResolvedAst, SymbolId};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::builder::FunctionBuilder;
use crate::{
    BinOp, BlockId, NirExpr, NirFunction, NirParam, NirParamRole, NirStmt, NirValue, Terminator,
};

pub(crate) fn lower(
    resolved: &ResolvedAst,
    plan: &DerivedDisplayPlan,
    function: FuncId,
    functions: &HashMap<SymbolId, FuncId>,
    derived: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
) -> NirFunction {
    if plan.mode == DisplayDerivationMode::LegacyStruct {
        return crate::native_derivation::lower(resolved, plan, function);
    }
    let mut builder = FunctionBuilder::new(function, plan.method_name);
    builder.function_type = plan.signature;
    builder.return_type = Intrinsic::Str.type_index();
    builder.display_owner = Some(plan.implementor);
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
    let mut emitter = DisplayEmitter {
        resolved,
        builder: &mut builder,
    };
    let result = emitter.aggregate(plan.implementor, NirValue::Local(receiver), &mut block);
    let result = emitter.value(
        block,
        NirExpr::TypeAssert(result, Intrinsic::Str.type_index()),
    );
    emitter.builder.blocks[block.0 as usize].terminator = Terminator::Return(result);
    builder.build()
}

struct DisplayEmitter<'a> {
    resolved: &'a ResolvedAst,
    builder: &'a mut FunctionBuilder,
}

impl DisplayEmitter<'_> {
    fn text(&self, text: &str) -> NirValue {
        NirValue::ConstStr(str_interner::intern(text))
    }
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
    fn append(&mut self, block: BlockId, left: NirValue, right: NirValue) -> NirValue {
        let value = self.value(
            block,
            NirExpr::CallBuiltin(runtime::ids::STR_CONCAT, vec![left, right]),
        );
        self.value(
            block,
            NirExpr::TypeAssert(value, Intrinsic::Str.type_index()),
        )
    }
    fn function(&self, ty: TypeIndex) -> Option<FuncId> {
        let trait_type = self.resolved.type_pool.well_known.display;
        let method = self.resolved.type_pool.find_trait_method(
            ty,
            trait_type,
            str_interner::intern("to_string"),
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
    fn component(
        &mut self,
        ty: TypeIndex,
        value: NirValue,
        quote_string: bool,
        block: &mut BlockId,
    ) -> NirValue {
        let ty = self
            .resolved
            .type_pool
            .canonical_type(ty)
            .expect("checked Display field type");
        if let Some(function) = self.function(ty) {
            let rendered = self.value(*block, NirExpr::Call(function, vec![value]));
            let rendered = self.value(
                *block,
                NirExpr::TypeAssert(rendered, Intrinsic::Str.type_index()),
            );
            return if quote_string
                && self.resolved.type_pool.as_intrinsic(ty) == Some(Intrinsic::Str)
            {
                let quoted = self.value(
                    *block,
                    NirExpr::CallBuiltin(runtime::ids::DISPLAY_QUOTE, vec![rendered]),
                );
                self.value(
                    *block,
                    NirExpr::TypeAssert(quoted, Intrinsic::Str.type_index()),
                )
            } else {
                rendered
            };
        }
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Optional { inner } => {
                if self.resolved.type_pool.as_intrinsic(inner) == Some(Intrinsic::NoReturn) {
                    return self.text("null");
                }
                let is_null = self.value(*block, NirExpr::BinOp(BinOp::Eq, value, NirValue::Null));
                let null = self.builder.new_block();
                let mut nonnull = self.builder.new_block();
                let join = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_null, null, nonnull);
                let result = self.builder.alloc_local();
                let null_text = self.text("null");
                self.builder.blocks[null.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(result, NirExpr::Use(null_text)));
                self.builder.blocks[null.0 as usize].terminator = Terminator::Goto(join);
                let rendered = self.component(inner, value, quote_string, &mut nonnull);
                self.builder.blocks[nonnull.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(result, NirExpr::Use(rendered)));
                self.builder.blocks[nonnull.0 as usize].terminator = Terminator::Goto(join);
                *block = join;
                NirValue::Local(result)
            }
            TypeKind::Tuple { .. } => self.inline_tuple(ty, value, block),
            _ => unreachable!("Display field prerequisite was checked before lowering"),
        }
    }
    fn inline_tuple(&mut self, ty: TypeIndex, value: NirValue, block: &mut BlockId) -> NirValue {
        let entered = self.value(
            *block,
            NirExpr::CallBuiltin(runtime::ids::DISPLAY_ENTER_TUPLE, vec![value]),
        );
        let entered = self.value(
            *block,
            NirExpr::TypeAssert(entered, Intrinsic::Bool.type_index()),
        );
        let mut body = self.builder.new_block();
        let cycle = self.builder.new_block();
        let join = self.builder.new_block();
        let result = self.builder.alloc_local();
        self.builder.blocks[block.0 as usize].terminator = Terminator::Branch(entered, body, cycle);
        let cycle_text = self.text("<cycle>");
        self.builder.blocks[cycle.0 as usize]
            .stmts
            .push(NirStmt::Assign(result, NirExpr::Use(cycle_text)));
        self.builder.blocks[cycle.0 as usize].terminator = Terminator::Goto(join);
        let rendered = self.aggregate(ty, value, &mut body);
        self.value(
            body,
            NirExpr::CallBuiltin(runtime::ids::DISPLAY_EXIT_TUPLE, vec![value]),
        );
        self.builder.blocks[body.0 as usize]
            .stmts
            .push(NirStmt::Assign(result, NirExpr::Use(rendered)));
        self.builder.blocks[body.0 as usize].terminator = Terminator::Goto(join);
        *block = join;
        NirValue::Local(result)
    }
    fn fields(
        &mut self,
        fields: &[(TypeIndex, Option<StrId>)],
        receiver: NirValue,
        enum_fields: bool,
        quote: bool,
        mut output: NirValue,
        block: &mut BlockId,
    ) -> NirValue {
        for (index, &(ty, name)) in fields.iter().enumerate() {
            if index != 0 {
                output = self.append(*block, output, self.text(", "));
            }
            if let Some(name) = name {
                output = self.append(
                    *block,
                    output,
                    self.text(&format!("{}: ", str_interner::get(name))),
                );
            }
            let expression = if enum_fields {
                NirExpr::EnumField(receiver, index as u32)
            } else {
                NirExpr::FieldAccess(receiver, index as u32)
            };
            let value = self.value(*block, expression);
            let rendered = self.component(ty, value, quote, block);
            output = self.append(*block, output, rendered);
        }
        output
    }
    fn aggregate(&mut self, ty: TypeIndex, receiver: NirValue, block: &mut BlockId) -> NirValue {
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Struct { name, fields } => {
                let output = self.text(&format!("{} {{ ", str_interner::get(name)));
                let fields = fields
                    .iter()
                    .map(|field| (field.ty, Some(field.name)))
                    .collect::<Vec<_>>();
                let output = self.fields(&fields, receiver, false, false, output, block);
                self.append(*block, output, self.text(" }"))
            }
            TypeKind::Tuple { elements } => {
                let fields = elements.iter().map(|&ty| (ty, None)).collect::<Vec<_>>();
                let output = self.fields(&fields, receiver, false, true, self.text("("), block);
                self.append(
                    *block,
                    output,
                    self.text(if elements.len() == 1 { ",)" } else { ")" }),
                )
            }
            TypeKind::Enum { name, variants } => {
                let result = self.builder.alloc_local();
                let join = self.builder.new_block();
                for variant in &variants {
                    let condition = self.value(*block, NirExpr::EnumIs(receiver, ty, variant.tag));
                    let mut selected = self.builder.new_block();
                    let next = self.builder.new_block();
                    self.builder.blocks[block.0 as usize].terminator =
                        Terminator::Branch(condition, selected, next);
                    let prefix = format!(
                        "{}.{}",
                        str_interner::get(name),
                        str_interner::get(variant.name)
                    );
                    let output = if variant.fields.is_empty() {
                        self.text(&prefix)
                    } else {
                        let fields = variant
                            .fields
                            .iter()
                            .map(|field| (field.ty, None))
                            .collect::<Vec<_>>();
                        let output = self.fields(
                            &fields,
                            receiver,
                            true,
                            true,
                            self.text(&format!("{prefix}(")),
                            &mut selected,
                        );
                        self.append(selected, output, self.text(")"))
                    };
                    self.builder.blocks[selected.0 as usize]
                        .stmts
                        .push(NirStmt::Assign(result, NirExpr::Use(output)));
                    self.builder.blocks[selected.0 as usize].terminator = Terminator::Goto(join);
                    *block = next;
                }
                self.builder.blocks[block.0 as usize].terminator = Terminator::MatchFail;
                *block = join;
                NirValue::Local(result)
            }
            _ => unreachable!("Display derivation target is a checked aggregate"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use type_pool::{TypeId, TypeInfo, VariantInfo};

    #[test]
    fn tuple_and_enum_bodies_publish_authenticated_ordinary_functions() {
        let (mut resolved, node) = crate::literal::tests::resolved_integer("42", false, "i64");
        let tuple = resolved
            .type_pool
            .intern_structural(TypeKind::Tuple { elements: vec![] });
        let enumeration = resolved.type_pool.register(TypeInfo {
            kind: TypeKind::Enum {
                name: str_interner::intern("Empty"),
                variants: vec![VariantInfo {
                    tag: 0,
                    name: str_interner::intern("only"),
                    fields: vec![],
                }],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 8,
        });
        for owner in [tuple, enumeration] {
            let signature = resolved.type_pool.intern_structural(TypeKind::Function {
                params: vec![owner],
                ret: Intrinsic::Str.type_index(),
            });
            let plan = DerivedDisplayPlan {
                mode: DisplayDerivationMode::FieldCalls,
                node,
                implementor: owner,
                trait_type: resolved.type_pool.well_known.display,
                method_name: str_interner::intern("to_string"),
                signature,
            };
            let function = lower(
                &resolved,
                &plan,
                FuncId(7),
                &HashMap::new(),
                &HashMap::new(),
            );
            assert_eq!(function.display_owner, Some(owner));
            assert_eq!(function.function_type, signature);
            assert_eq!(
                function.entry_abi().parameters,
                vec![nsbc::ParameterAbi::Value]
            );
            assert!(function.blocks.iter().flat_map(|block| &block.stmts).all(|statement| {
                !matches!(statement, NirStmt::Assign(_, expression) if matches!(expression.unscoped(), NirExpr::CallBuiltin(id, _) if *id == runtime::ids::DERIVED_DISPLAY))
            }));
            assert!(
                function
                    .blocks
                    .iter()
                    .any(|block| matches!(block.terminator, Terminator::Return(_)))
            );
            if owner == enumeration {
                assert!(function.blocks.iter().flat_map(|block| &block.stmts).any(|statement| {
                    matches!(statement, NirStmt::Assign(_, expression) if matches!(expression.unscoped(), NirExpr::EnumIs(_, ty, 0) if *ty == owner))
                }));
            }
        }
    }
}
