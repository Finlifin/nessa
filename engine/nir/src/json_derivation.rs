//! Compositional `Json` serializer (`to_json`) and deserializer (`from_json`) lowering.
//!
//! Each `derive Json for T` lowers into ordinary checked NIR functions backed by
//! the VM's host-side `JsonTape` and `JsonWriter` handle primitives:
//!   - `__json_write_<T>(val: T, writer: i64) -> !JsonError Unit`
//!   - `<T>.to_json(self: T) -> !JsonError String`
//!   - `__json_read_<T>(tape: i64, node: i64, path: String, allow_unknown: bool) -> !JsonError T`
//!   - `<T>.from_json(input: String, allow_unknown_fields: bool) -> !JsonError T`

use std::collections::HashMap;

use nsbc::{FuncId, GlobalId};
use resolution::{DerivedJsonPlan, ResolvedAst, SymbolId};
use str_interner::StrId;
use type_pool::{DERIVE_FUNC_ID, Intrinsic, TypeIndex, TypeKind};

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{
    BinOp, BlockId, DerivedMethod, NirExpr, NirFunction, NirParam, NirParamRole, NirStmt, NirValue,
    Terminator,
};

const WRITER_OP_BEGIN_OBJ: i64 = 1;
const WRITER_OP_END_OBJ: i64 = 2;
const WRITER_OP_BEGIN_ARR: i64 = 3;
const WRITER_OP_END_ARR: i64 = 4;
const WRITER_OP_FIELD_KEY: i64 = 5;
const WRITER_OP_SCALAR: i64 = 6;
const WRITER_OP_RAW_JSON: i64 = 7;

const TAPE_KIND_NULL: i64 = 1;
const TAPE_KIND_STR: i64 = 5;
const TAPE_KIND_ARR: i64 = 6;
const TAPE_KIND_OBJ: i64 = 7;

const JSON_ERR_UNEXPECTED_EOF: i64 = 1;
const JSON_ERR_INVALID_NUMBER: i64 = 3;
const JSON_ERR_INVALID_STRING: i64 = 4;
const JSON_ERR_INVALID_ESCAPE: i64 = 5;
const JSON_ERR_TRAILING_CHARS: i64 = 6;
const JSON_ERR_DEPTH_EXCEEDED: i64 = 8;
const JSON_ERR_INT_OUT_OF_RANGE: i64 = 13;
const JSON_ERR_CYCLE_DETECTED: i64 = 14;

const VARIANT_TAG_UNEXPECTED_EOF: u32 = 0;
const VARIANT_TAG_UNEXPECTED_TOKEN: u32 = 1;
const VARIANT_TAG_INVALID_NUMBER: u32 = 2;
const VARIANT_TAG_INVALID_STRING: u32 = 3;
const VARIANT_TAG_INVALID_ESCAPE: u32 = 4;
const VARIANT_TAG_TRAILING_CHARS: u32 = 5;
const VARIANT_TAG_UNSUPPORTED_VALUE: u32 = 6;
const VARIANT_TAG_DEPTH_EXCEEDED: u32 = 7;
const VARIANT_TAG_TYPE_MISMATCH: u32 = 8;
const VARIANT_TAG_MISSING_FIELD: u32 = 9;
const VARIANT_TAG_UNKNOWN_FIELD: u32 = 10;
const VARIANT_TAG_INVALID_VARIANT: u32 = 11;
const VARIANT_TAG_INT_OUT_OF_RANGE: u32 = 12;
const VARIANT_TAG_CYCLE_DETECTED: u32 = 13;

#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_all(
    resolved: &ResolvedAst,
    next_func_id: &mut u32,
    func_map: &HashMap<SymbolId, FuncId>,
    derived_functions: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
    global_map: &HashMap<SymbolId, GlobalId>,
    write_helpers: &HashMap<TypeIndex, FuncId>,
    read_helpers: &HashMap<TypeIndex, FuncId>,
    out_functions: &mut Vec<NirFunction>,
    out_derived_methods: &mut Vec<DerivedMethod>,
) {
    for plan in &resolved.json_derivations {
        lower_plan(
            resolved,
            plan,
            next_func_id,
            func_map,
            derived_functions,
            global_map,
            write_helpers,
            read_helpers,
            out_functions,
            out_derived_methods,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_plan(
    resolved: &ResolvedAst,
    plan: &DerivedJsonPlan,
    next_func_id: &mut u32,
    func_map: &HashMap<SymbolId, FuncId>,
    derived_functions: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
    global_map: &HashMap<SymbolId, GlobalId>,
    write_helpers: &HashMap<TypeIndex, FuncId>,
    read_helpers: &HashMap<TypeIndex, FuncId>,
    out_functions: &mut Vec<NirFunction>,
    out_derived_methods: &mut Vec<DerivedMethod>,
) {
    let to_json_fn = derived_functions[&(plan.implementor, plan.trait_type, plan.method_name)];
    let write_fn = write_helpers[&plan.implementor];
    let read_fn = read_helpers[&plan.implementor];

    out_functions.push(lower_write_helper(
        resolved,
        plan,
        write_fn,
        func_map,
        derived_functions,
        global_map,
        write_helpers,
    ));
    out_functions.push(lower_to_json(
        resolved,
        plan,
        to_json_fn,
        write_fn,
        func_map,
        derived_functions,
        global_map,
    ));

    let (read_nir, mut lambdas) = lower_read_helper(
        resolved,
        plan,
        read_fn,
        next_func_id,
        func_map,
        derived_functions,
        global_map,
        read_helpers,
    );
    out_functions.append(&mut lambdas);
    out_functions.push(read_nir);

    if let Some(sym) = plan.from_json_symbol {
        let from_json_fn = func_map[&sym];
        out_functions.push(lower_from_json(
            resolved,
            plan,
            from_json_fn,
            read_fn,
            func_map,
            derived_functions,
            global_map,
        ));
    }

    out_derived_methods.push(DerivedMethod {
        implementor: plan.implementor,
        trait_type: plan.trait_type,
        method_name: plan.method_name,
        func_id: to_json_fn,
    });
}

fn lower_write_helper(
    resolved: &ResolvedAst,
    plan: &DerivedJsonPlan,
    func_id: FuncId,
    func_map: &HashMap<SymbolId, FuncId>,
    derived_functions: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
    global_map: &HashMap<SymbolId, GlobalId>,
    write_helpers: &HashMap<TypeIndex, FuncId>,
) -> NirFunction {
    let name = str_interner::intern(&format!("__json_write_{}", plan.implementor.as_u32()));
    let mut builder = FunctionBuilder::new(func_id, name);
    builder.function_type = plan.nir_types.write_helper_fn_type;
    builder.return_type = plan.nir_types.err_unit_type;
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|s| s.0);
    builder.func_map = func_map.clone();
    builder.derived_functions = derived_functions.clone();
    builder.global_map = global_map.clone();

    let val_local = builder.alloc_local();
    let writer_local = builder.alloc_local();
    builder.params.push(NirParam {
        local: val_local,
        name: str_interner::intern("val"),
        type_index: plan.implementor,
        role: NirParamRole::User,
    });
    builder.params.push(NirParam {
        local: writer_local,
        name: str_interner::intern("writer"),
        type_index: Intrinsic::I64.type_index(),
        role: NirParamRole::User,
    });

    let mut block = builder.new_block();
    builder.entry_block = block;

    let mut emitter = JsonWriteEmitter {
        resolved,
        plan,
        builder: &mut builder,
        write_helpers,
    };
    emitter.emit_aggregate(
        plan.implementor,
        NirValue::Local(val_local),
        NirValue::Local(writer_local),
        &mut block,
    );
    let ok_val = emitter.value(
        block,
        NirExpr::ErrorOk(NirValue::Unit, plan.nir_types.err_unit_type),
    );
    emitter.builder.blocks[block.0 as usize].terminator = Terminator::Return(ok_val);
    builder.build()
}

fn lower_to_json(
    resolved: &ResolvedAst,
    plan: &DerivedJsonPlan,
    func_id: FuncId,
    write_fn: FuncId,
    func_map: &HashMap<SymbolId, FuncId>,
    derived_functions: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
    global_map: &HashMap<SymbolId, GlobalId>,
) -> NirFunction {
    let mut builder = FunctionBuilder::new(func_id, plan.method_name);
    builder.function_type = plan.signature;
    builder.return_type = plan.nir_types.err_str_type;
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|s| s.0);
    builder.func_map = func_map.clone();
    builder.derived_functions = derived_functions.clone();
    builder.global_map = global_map.clone();

    let receiver = builder.alloc_local();
    builder.params.push(NirParam {
        local: receiver,
        name: str_interner::intern("self"),
        type_index: plan.implementor,
        role: NirParamRole::User,
    });

    let entry = builder.new_block();
    builder.entry_block = entry;

    let empty_helpers = HashMap::new();
    let mut emitter = JsonWriteEmitter {
        resolved,
        plan,
        builder: &mut builder,
        write_helpers: &empty_helpers,
    };

    let raw_w = emitter.value(
        entry,
        NirExpr::CallBuiltin(runtime::ids::JSON_WRITER_NEW, vec![NirValue::ConstInt(0)]),
    );
    let writer = emitter.value(
        entry,
        NirExpr::TypeAssert(raw_w, Intrinsic::I64.type_index()),
    );
    let wres = emitter.value(
        entry,
        NirExpr::Call(write_fn, vec![NirValue::Local(receiver), writer]),
    );
    let is_ok = emitter.value(entry, NirExpr::ErrorIsOk(wres));

    let finish_b = emitter.builder.new_block();
    let werr_b = emitter.builder.new_block();
    emitter.builder.blocks[entry.0 as usize].terminator =
        Terminator::Branch(is_ok, finish_b, werr_b);

    let _ = emitter.value(
        werr_b,
        NirExpr::CallBuiltin(runtime::ids::JSON_WRITER_CLOSE, vec![writer]),
    );
    let err_payload = emitter.value(werr_b, NirExpr::ErrorPayload(wres));
    let err_ret = emitter.value(
        werr_b,
        NirExpr::ErrorErr(err_payload, plan.nir_types.err_str_type),
    );
    emitter.builder.blocks[werr_b.0 as usize].terminator = Terminator::Return(err_ret);

    let fin_raw = emitter.value(
        finish_b,
        NirExpr::CallBuiltin(runtime::ids::JSON_WRITER_FINISH, vec![writer]),
    );
    let fin_tup = emitter.value(
        finish_b,
        NirExpr::TypeAssert(fin_raw, plan.nir_types.tuple_i64_str_str),
    );
    let _ = emitter.value(
        finish_b,
        NirExpr::CallBuiltin(runtime::ids::JSON_WRITER_CLOSE, vec![writer]),
    );
    let code = emitter.value(finish_b, NirExpr::FieldAccess(fin_tup, 0));
    let msg = emitter.value(finish_b, NirExpr::FieldAccess(fin_tup, 1));
    let out = emitter.value(finish_b, NirExpr::FieldAccess(fin_tup, 2));

    let code_ok = emitter.value(
        finish_b,
        NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(0)),
    );
    let ret_ok_b = emitter.builder.new_block();
    let mut ret_err_b = emitter.builder.new_block();
    emitter.builder.blocks[finish_b.0 as usize].terminator =
        Terminator::Branch(code_ok, ret_ok_b, ret_err_b);

    let out_str = emitter.value(
        ret_ok_b,
        NirExpr::TypeAssert(out, Intrinsic::Str.type_index()),
    );
    let ok_ret = emitter.value(
        ret_ok_b,
        NirExpr::ErrorOk(out_str, plan.nir_types.err_str_type),
    );
    emitter.builder.blocks[ret_ok_b.0 as usize].terminator = Terminator::Return(ok_ret);

    let msg_str = emitter.value(
        ret_err_b,
        NirExpr::TypeAssert(msg, Intrinsic::Str.type_index()),
    );
    let err_enum = emitter.build_writer_error(code, Some(msg_str), &mut ret_err_b);
    let fin_err_ret = emitter.value(
        ret_err_b,
        NirExpr::ErrorErr(err_enum, plan.nir_types.err_str_type),
    );
    emitter.builder.blocks[ret_err_b.0 as usize].terminator = Terminator::Return(fin_err_ret);

    builder.build()
}

#[allow(clippy::too_many_arguments)]
fn lower_read_helper(
    resolved: &ResolvedAst,
    plan: &DerivedJsonPlan,
    func_id: FuncId,
    next_func_id: &mut u32,
    func_map: &HashMap<SymbolId, FuncId>,
    derived_functions: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
    global_map: &HashMap<SymbolId, GlobalId>,
    read_helpers: &HashMap<TypeIndex, FuncId>,
) -> (NirFunction, Vec<NirFunction>) {
    let name = str_interner::intern(&format!("__json_read_{}", plan.implementor.as_u32()));
    let mut builder = FunctionBuilder::new(func_id, name);
    builder.function_type = plan.nir_types.read_helper_fn_type;
    builder.return_type = plan.nir_types.err_self_type;
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|s| s.0);
    builder.func_map = func_map.clone();
    builder.derived_functions = derived_functions.clone();
    builder.global_map = global_map.clone();
    builder.next_func_id = *next_func_id;

    let tape_local = builder.alloc_local();
    let node_local = builder.alloc_local();
    let path_local = builder.alloc_local();
    let allow_unknown_local = builder.alloc_local();
    builder.params.push(NirParam {
        local: tape_local,
        name: str_interner::intern("tape"),
        type_index: Intrinsic::I64.type_index(),
        role: NirParamRole::User,
    });
    builder.params.push(NirParam {
        local: node_local,
        name: str_interner::intern("node"),
        type_index: Intrinsic::I64.type_index(),
        role: NirParamRole::User,
    });
    builder.params.push(NirParam {
        local: path_local,
        name: str_interner::intern("path"),
        type_index: Intrinsic::Str.type_index(),
        role: NirParamRole::User,
    });
    builder.params.push(NirParam {
        local: allow_unknown_local,
        name: str_interner::intern("allow_unknown"),
        type_index: Intrinsic::Bool.type_index(),
        role: NirParamRole::User,
    });

    let mut block = builder.new_block();
    builder.entry_block = block;

    let mut emitter = JsonReadEmitter {
        resolved,
        plan,
        builder: &mut builder,
        read_helpers,
        tape: NirValue::Local(tape_local),
        allow_unknown: NirValue::Local(allow_unknown_local),
    };
    let decoded = emitter.emit_aggregate(
        plan.implementor,
        NirValue::Local(node_local),
        NirValue::Local(path_local),
        &mut block,
    );
    let ok_val = emitter.value(
        block,
        NirExpr::ErrorOk(decoded, plan.nir_types.err_self_type),
    );
    emitter.builder.blocks[block.0 as usize].terminator = Terminator::Return(ok_val);

    *next_func_id = builder.next_func_id;
    let lambdas = std::mem::take(&mut builder.lambda_functions);
    (builder.build(), lambdas)
}

fn lower_from_json(
    resolved: &ResolvedAst,
    plan: &DerivedJsonPlan,
    func_id: FuncId,
    read_fn: FuncId,
    func_map: &HashMap<SymbolId, FuncId>,
    derived_functions: &HashMap<(TypeIndex, TypeIndex, StrId), FuncId>,
    global_map: &HashMap<SymbolId, GlobalId>,
) -> NirFunction {
    let mut builder = FunctionBuilder::new(func_id, str_interner::intern("from_json"));
    builder.function_type = plan.nir_types.from_json_fn_type;
    builder.return_type = plan.nir_types.err_self_type;
    builder.entry_scope = resolved.node_scopes.get(&plan.node).map(|s| s.0);
    builder.func_map = func_map.clone();
    builder.derived_functions = derived_functions.clone();
    builder.global_map = global_map.clone();

    let input_local = builder.alloc_local();
    let allow_unknown_local = builder.alloc_local();
    builder.params.push(NirParam {
        local: input_local,
        name: str_interner::intern("input"),
        type_index: Intrinsic::Str.type_index(),
        role: NirParamRole::User,
    });
    builder.params.push(NirParam {
        local: allow_unknown_local,
        name: str_interner::intern("allow_unknown_fields"),
        type_index: Intrinsic::Bool.type_index(),
        role: NirParamRole::User,
    });

    let entry = builder.new_block();
    builder.entry_block = entry;

    let empty_helpers = HashMap::new();
    let mut emitter = JsonReadEmitter {
        resolved,
        plan,
        builder: &mut builder,
        read_helpers: &empty_helpers,
        tape: NirValue::ConstInt(0),
        allow_unknown: NirValue::Local(allow_unknown_local),
    };

    let open_raw = emitter.value(
        entry,
        NirExpr::CallBuiltin(
            runtime::ids::JSON_TAPE_OPEN,
            vec![NirValue::Local(input_local)],
        ),
    );
    let open_tup = emitter.value(
        entry,
        NirExpr::TypeAssert(open_raw, plan.nir_types.tuple_i64_i64_str),
    );
    let code = emitter.value(entry, NirExpr::FieldAccess(open_tup, 0));
    let tape_or_off = emitter.value(entry, NirExpr::FieldAccess(open_tup, 1));
    let msg = emitter.value(entry, NirExpr::FieldAccess(open_tup, 2));

    let is_ok = emitter.value(
        entry,
        NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(0)),
    );
    let decode_b = emitter.builder.new_block();
    let mut syntax_err_b = emitter.builder.new_block();
    emitter.builder.blocks[entry.0 as usize].terminator =
        Terminator::Branch(is_ok, decode_b, syntax_err_b);

    let off_i64 = emitter.value(
        syntax_err_b,
        NirExpr::TypeAssert(tape_or_off, Intrinsic::I64.type_index()),
    );
    let msg_str = emitter.value(
        syntax_err_b,
        NirExpr::TypeAssert(msg, Intrinsic::Str.type_index()),
    );
    let syntax_err = emitter.build_syntax_error(code, off_i64, msg_str, &mut syntax_err_b);
    emitter.emit_return_err(syntax_err, syntax_err_b);

    let tape_i64 = emitter.value(
        decode_b,
        NirExpr::TypeAssert(tape_or_off, Intrinsic::I64.type_index()),
    );
    let root_path = NirValue::ConstStr(str_interner::intern("$"));
    let res = emitter.value(
        decode_b,
        NirExpr::Call(
            read_fn,
            vec![
                tape_i64,
                NirValue::ConstInt(0),
                root_path,
                NirValue::Local(allow_unknown_local),
            ],
        ),
    );
    let _ = emitter.value(
        decode_b,
        NirExpr::CallBuiltin(runtime::ids::JSON_TAPE_CLOSE, vec![tape_i64]),
    );
    emitter.builder.blocks[decode_b.0 as usize].terminator = Terminator::Return(res);

    builder.build()
}

struct JsonWriteEmitter<'a> {
    resolved: &'a ResolvedAst,
    plan: &'a DerivedJsonPlan,
    builder: &'a mut FunctionBuilder,
    write_helpers: &'a HashMap<TypeIndex, FuncId>,
}

impl JsonWriteEmitter<'_> {
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

    fn build_writer_error(
        &mut self,
        code: NirValue,
        msg_opt: Option<NirValue>,
        block: &mut BlockId,
    ) -> NirValue {
        let err_ty = self.plan.json_error_type;
        let is_depth = self.value(
            *block,
            NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(JSON_ERR_DEPTH_EXCEEDED)),
        );
        let depth_b = self.builder.new_block();
        let check_cycle_b = self.builder.new_block();
        let join_b = self.builder.new_block();
        let result = self.builder.alloc_local();

        self.builder.blocks[block.0 as usize].terminator =
            Terminator::Branch(is_depth, depth_b, check_cycle_b);

        self.builder.blocks[depth_b.0 as usize]
            .stmts
            .push(NirStmt::Assign(
                result,
                NirExpr::Use(NirValue::ConstEnum(err_ty, VARIANT_TAG_DEPTH_EXCEEDED)),
            ));
        self.builder.blocks[depth_b.0 as usize].terminator = Terminator::Goto(join_b);

        let is_cycle = self.value(
            check_cycle_b,
            NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(JSON_ERR_CYCLE_DETECTED)),
        );
        let cycle_b = self.builder.new_block();
        let unsup_b = self.builder.new_block();
        self.builder.blocks[check_cycle_b.0 as usize].terminator =
            Terminator::Branch(is_cycle, cycle_b, unsup_b);

        self.builder.blocks[cycle_b.0 as usize]
            .stmts
            .push(NirStmt::Assign(
                result,
                NirExpr::Use(NirValue::ConstEnum(err_ty, VARIANT_TAG_CYCLE_DETECTED)),
            ));
        self.builder.blocks[cycle_b.0 as usize].terminator = Terminator::Goto(join_b);

        let msg_val = msg_opt.unwrap_or_else(|| {
            NirValue::ConstStr(str_interner::intern(
                "non-finite float (NaN or Infinity) is not valid JSON",
            ))
        });
        let tup = self.value(
            unsup_b,
            NirExpr::NewObject(self.plan.nir_types.tuple_str, vec![msg_val]),
        );
        let unsup_enum = self.value(
            unsup_b,
            NirExpr::NewEnum(err_ty, VARIANT_TAG_UNSUPPORTED_VALUE, tup),
        );
        self.builder.blocks[unsup_b.0 as usize]
            .stmts
            .push(NirStmt::Assign(result, NirExpr::Use(unsup_enum)));
        self.builder.blocks[unsup_b.0 as usize].terminator = Terminator::Goto(join_b);

        *block = join_b;
        NirValue::Local(result)
    }

    fn emit_writer_op(&mut self, writer: NirValue, op: i64, arg: NirValue, block: &mut BlockId) {
        let raw = self.value(
            *block,
            NirExpr::CallBuiltin(
                runtime::ids::JSON_WRITER_OP,
                vec![writer, NirValue::ConstInt(op), arg],
            ),
        );
        let status = self.value(
            *block,
            NirExpr::TypeAssert(raw, Intrinsic::I64.type_index()),
        );
        let ok = self.value(
            *block,
            NirExpr::BinOp(BinOp::Eq, status, NirValue::ConstInt(0)),
        );
        let cont_b = self.builder.new_block();
        let mut err_b = self.builder.new_block();
        self.builder.blocks[block.0 as usize].terminator = Terminator::Branch(ok, cont_b, err_b);

        let err_enum = self.build_writer_error(status, None, &mut err_b);
        let err_ret = self.value(
            err_b,
            NirExpr::ErrorErr(err_enum, self.plan.nir_types.err_unit_type),
        );
        self.builder.blocks[err_b.0 as usize].terminator = Terminator::Return(err_ret);

        *block = cont_b;
    }

    fn custom_to_json_fn(&self, canon: TypeIndex) -> Option<FuncId> {
        let slot = self.resolved.type_pool.find_trait_method(
            canon,
            self.resolved.type_pool.well_known.json,
            str_interner::intern("to_json"),
        )?;
        if slot.func_id == DERIVE_FUNC_ID {
            None
        } else {
            self.builder.func_map.get(&SymbolId(slot.func_id)).copied()
        }
    }

    fn emit_component(
        &mut self,
        ty: TypeIndex,
        val: NirValue,
        writer: NirValue,
        block: &mut BlockId,
    ) {
        let canon = self
            .resolved
            .type_pool
            .canonical_type(ty)
            .expect("checked Json component type");

        if let Some(&helper_fn) = self.write_helpers.get(&canon) {
            let res = self.value(*block, NirExpr::Call(helper_fn, vec![val, writer]));
            let is_ok = self.value(*block, NirExpr::ErrorIsOk(res));
            let cont_b = self.builder.new_block();
            let err_b = self.builder.new_block();
            self.builder.blocks[block.0 as usize].terminator =
                Terminator::Branch(is_ok, cont_b, err_b);
            self.builder.blocks[err_b.0 as usize].terminator = Terminator::Return(res);
            *block = cont_b;
            return;
        }

        if self.resolved.type_pool.as_intrinsic(canon).is_some() {
            self.emit_writer_op(writer, WRITER_OP_SCALAR, val, block);
            return;
        }

        if let Some(custom_fn) = self.custom_to_json_fn(canon) {
            let res = self.value(*block, NirExpr::Call(custom_fn, vec![val]));
            let is_ok = self.value(*block, NirExpr::ErrorIsOk(res));
            let mut ok_b = self.builder.new_block();
            let err_b = self.builder.new_block();
            self.builder.blocks[block.0 as usize].terminator =
                Terminator::Branch(is_ok, ok_b, err_b);

            let ep = self.value(err_b, NirExpr::ErrorPayload(res));
            let er = self.value(
                err_b,
                NirExpr::ErrorErr(ep, self.plan.nir_types.err_unit_type),
            );
            self.builder.blocks[err_b.0 as usize].terminator = Terminator::Return(er);

            let raw = self.value(ok_b, NirExpr::ErrorPayload(res));
            let raw_str = self.value(ok_b, NirExpr::TypeAssert(raw, Intrinsic::Str.type_index()));
            self.emit_writer_op(writer, WRITER_OP_RAW_JSON, raw_str, &mut ok_b);
            *block = ok_b;
            return;
        }

        match self.resolved.type_pool.get(canon).kind.clone() {
            TypeKind::Optional { inner } => {
                if self.resolved.type_pool.as_intrinsic(inner) == Some(Intrinsic::NoReturn) {
                    self.emit_writer_op(writer, WRITER_OP_SCALAR, NirValue::Null, block);
                    return;
                }
                let is_null = self.value(*block, NirExpr::BinOp(BinOp::Eq, val, NirValue::Null));
                let mut null_b = self.builder.new_block();
                let mut some_b = self.builder.new_block();
                let join_b = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_null, null_b, some_b);

                self.emit_writer_op(writer, WRITER_OP_SCALAR, NirValue::Null, &mut null_b);
                self.builder.blocks[null_b.0 as usize].terminator = Terminator::Goto(join_b);

                self.emit_component(inner, val, writer, &mut some_b);
                self.builder.blocks[some_b.0 as usize].terminator = Terminator::Goto(join_b);

                *block = join_b;
            }
            TypeKind::Tuple { elements } => {
                self.emit_writer_op(writer, WRITER_OP_BEGIN_ARR, val, block);
                for (idx, &elem_ty) in elements.iter().enumerate() {
                    let elem_val = self.value(*block, NirExpr::FieldAccess(val, idx as u32));
                    self.emit_component(elem_ty, elem_val, writer, block);
                }
                self.emit_writer_op(writer, WRITER_OP_END_ARR, NirValue::Unit, block);
            }
            _ => unreachable!("Json write prerequisite was validated during resolution"),
        }
    }

    fn emit_aggregate(
        &mut self,
        ty: TypeIndex,
        val: NirValue,
        writer: NirValue,
        block: &mut BlockId,
    ) {
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Struct { fields, .. } => {
                self.emit_writer_op(writer, WRITER_OP_BEGIN_OBJ, val, block);
                for (idx, field) in fields.iter().enumerate() {
                    self.emit_writer_op(
                        writer,
                        WRITER_OP_FIELD_KEY,
                        NirValue::ConstStr(field.name),
                        block,
                    );
                    let fval = self.value(*block, NirExpr::FieldAccess(val, idx as u32));
                    self.emit_component(field.ty, fval, writer, block);
                }
                self.emit_writer_op(writer, WRITER_OP_END_OBJ, NirValue::Unit, block);
            }
            TypeKind::Enum { variants, .. } => {
                let join_b = self.builder.new_block();
                for variant in &variants {
                    let cond = self.value(*block, NirExpr::EnumIs(val, ty, variant.tag));
                    let mut vblock = self.builder.new_block();
                    let next_b = self.builder.new_block();
                    self.builder.blocks[block.0 as usize].terminator =
                        Terminator::Branch(cond, vblock, next_b);

                    let vname = NirValue::ConstStr(variant.name);
                    match variant.fields.len() {
                        0 => {
                            self.emit_writer_op(writer, WRITER_OP_SCALAR, vname, &mut vblock);
                        }
                        1 => {
                            self.emit_writer_op(writer, WRITER_OP_BEGIN_OBJ, val, &mut vblock);
                            self.emit_writer_op(writer, WRITER_OP_FIELD_KEY, vname, &mut vblock);
                            let f0 = self.value(vblock, NirExpr::EnumField(val, 0));
                            self.emit_component(variant.fields[0].ty, f0, writer, &mut vblock);
                            self.emit_writer_op(
                                writer,
                                WRITER_OP_END_OBJ,
                                NirValue::Unit,
                                &mut vblock,
                            );
                        }
                        _ => {
                            self.emit_writer_op(writer, WRITER_OP_BEGIN_OBJ, val, &mut vblock);
                            self.emit_writer_op(writer, WRITER_OP_FIELD_KEY, vname, &mut vblock);
                            self.emit_writer_op(
                                writer,
                                WRITER_OP_BEGIN_ARR,
                                NirValue::Unit,
                                &mut vblock,
                            );
                            for (idx, field) in variant.fields.iter().enumerate() {
                                let fi = self.value(vblock, NirExpr::EnumField(val, idx as u32));
                                self.emit_component(field.ty, fi, writer, &mut vblock);
                            }
                            self.emit_writer_op(
                                writer,
                                WRITER_OP_END_ARR,
                                NirValue::Unit,
                                &mut vblock,
                            );
                            self.emit_writer_op(
                                writer,
                                WRITER_OP_END_OBJ,
                                NirValue::Unit,
                                &mut vblock,
                            );
                        }
                    }

                    self.builder.blocks[vblock.0 as usize].terminator = Terminator::Goto(join_b);
                    *block = next_b;
                }
                self.builder.blocks[block.0 as usize].terminator = Terminator::MatchFail;
                *block = join_b;
            }
            TypeKind::Newtype { inner, .. } => {
                let inner_val = self.value(*block, NirExpr::FieldAccess(val, 0));
                self.emit_component(inner, inner_val, writer, block);
            }
            TypeKind::Tuple { elements } => {
                self.emit_writer_op(writer, WRITER_OP_BEGIN_ARR, val, block);
                for (idx, &elem_ty) in elements.iter().enumerate() {
                    let ev = self.value(*block, NirExpr::FieldAccess(val, idx as u32));
                    self.emit_component(elem_ty, ev, writer, block);
                }
                self.emit_writer_op(writer, WRITER_OP_END_ARR, NirValue::Unit, block);
            }
            _ => unreachable!("Json derive target was validated during resolution"),
        }
    }
}

struct JsonReadEmitter<'a> {
    resolved: &'a ResolvedAst,
    plan: &'a DerivedJsonPlan,
    builder: &'a mut FunctionBuilder,
    read_helpers: &'a HashMap<TypeIndex, FuncId>,
    tape: NirValue,
    allow_unknown: NirValue,
}

impl JsonReadEmitter<'_> {
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

    fn append_str(&mut self, block: BlockId, left: NirValue, right: NirValue) -> NirValue {
        let raw = self.value(
            block,
            NirExpr::CallBuiltin(runtime::ids::STR_CONCAT, vec![left, right]),
        );
        self.value(block, NirExpr::TypeAssert(raw, Intrinsic::Str.type_index()))
    }

    fn emit_return_err(&mut self, err_enum: NirValue, block: BlockId) {
        let wrapped = self.value(
            block,
            NirExpr::ErrorErr(err_enum, self.plan.nir_types.err_self_type),
        );
        self.builder.blocks[block.0 as usize].terminator = Terminator::Return(wrapped);
    }

    fn emit_type_mismatch(
        &mut self,
        path_val: NirValue,
        expected: &str,
        actual_val: NirValue,
        block: BlockId,
    ) {
        let exp_v = NirValue::ConstStr(str_interner::intern(expected));
        let tup = self.value(
            block,
            NirExpr::NewObject(
                self.plan.nir_types.tuple_str3,
                vec![path_val, exp_v, actual_val],
            ),
        );
        let err_enum = self.value(
            block,
            NirExpr::NewEnum(self.plan.json_error_type, VARIANT_TAG_TYPE_MISMATCH, tup),
        );
        self.emit_return_err(err_enum, block);
    }

    fn emit_missing_field(&mut self, path_val: NirValue, field_name: StrId, block: BlockId) {
        let tup = self.value(
            block,
            NirExpr::NewObject(
                self.plan.nir_types.tuple_str2,
                vec![path_val, NirValue::ConstStr(field_name)],
            ),
        );
        let err_enum = self.value(
            block,
            NirExpr::NewEnum(self.plan.json_error_type, VARIANT_TAG_MISSING_FIELD, tup),
        );
        self.emit_return_err(err_enum, block);
    }

    fn emit_unknown_field(&mut self, path_val: NirValue, field_val: NirValue, block: BlockId) {
        let tup = self.value(
            block,
            NirExpr::NewObject(self.plan.nir_types.tuple_str2, vec![path_val, field_val]),
        );
        let err_enum = self.value(
            block,
            NirExpr::NewEnum(self.plan.json_error_type, VARIANT_TAG_UNKNOWN_FIELD, tup),
        );
        self.emit_return_err(err_enum, block);
    }

    fn emit_invalid_variant(
        &mut self,
        path_val: NirValue,
        enum_type_name: &str,
        variant_val: NirValue,
        block: BlockId,
    ) {
        let tn = NirValue::ConstStr(str_interner::intern(enum_type_name));
        let tup = self.value(
            block,
            NirExpr::NewObject(
                self.plan.nir_types.tuple_str3,
                vec![path_val, tn, variant_val],
            ),
        );
        let err_enum = self.value(
            block,
            NirExpr::NewEnum(self.plan.json_error_type, VARIANT_TAG_INVALID_VARIANT, tup),
        );
        self.emit_return_err(err_enum, block);
    }

    fn emit_int_out_of_range(
        &mut self,
        path_val: NirValue,
        raw_tok_val: NirValue,
        target_type_name: &str,
        block: BlockId,
    ) {
        let tn = NirValue::ConstStr(str_interner::intern(target_type_name));
        let tup = self.value(
            block,
            NirExpr::NewObject(
                self.plan.nir_types.tuple_str3,
                vec![path_val, raw_tok_val, tn],
            ),
        );
        let err_enum = self.value(
            block,
            NirExpr::NewEnum(self.plan.json_error_type, VARIANT_TAG_INT_OUT_OF_RANGE, tup),
        );
        self.emit_return_err(err_enum, block);
    }

    fn emit_tape_kind(&mut self, node: NirValue, block: BlockId) -> NirValue {
        let raw = self.value(
            block,
            NirExpr::CallBuiltin(runtime::ids::JSON_TAPE_KIND, vec![self.tape, node]),
        );
        self.value(block, NirExpr::TypeAssert(raw, Intrinsic::I64.type_index()))
    }

    fn emit_kind_name(&mut self, kind: NirValue, block: &mut BlockId) -> NirValue {
        let cases: &[(i64, &str)] = &[
            (1, "null"),
            (2, "bool"),
            (3, "integer"),
            (4, "float"),
            (5, "string"),
            (6, "array"),
            (7, "object"),
        ];
        let result = self.builder.alloc_local();
        let join_b = self.builder.new_block();
        for &(tag, label) in cases {
            let cond = self.value(
                *block,
                NirExpr::BinOp(BinOp::Eq, kind, NirValue::ConstInt(tag)),
            );
            let hit_b = self.builder.new_block();
            let next_b = self.builder.new_block();
            self.builder.blocks[block.0 as usize].terminator =
                Terminator::Branch(cond, hit_b, next_b);
            self.builder.blocks[hit_b.0 as usize]
                .stmts
                .push(NirStmt::Assign(
                    result,
                    NirExpr::Use(NirValue::ConstStr(str_interner::intern(label))),
                ));
            self.builder.blocks[hit_b.0 as usize].terminator = Terminator::Goto(join_b);
            *block = next_b;
        }
        self.builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(
                result,
                NirExpr::Use(NirValue::ConstStr(str_interner::intern("invalid"))),
            ));
        self.builder.blocks[block.0 as usize].terminator = Terminator::Goto(join_b);
        *block = join_b;
        NirValue::Local(result)
    }

    fn build_syntax_error(
        &mut self,
        code: NirValue,
        offset: NirValue,
        msg: NirValue,
        block: &mut BlockId,
    ) -> NirValue {
        let err_ty = self.plan.json_error_type;
        let result = self.builder.alloc_local();
        let join_b = self.builder.new_block();

        let cases: &[(i64, u32, bool)] = &[
            (JSON_ERR_UNEXPECTED_EOF, VARIANT_TAG_UNEXPECTED_EOF, false),
            (JSON_ERR_INVALID_NUMBER, VARIANT_TAG_INVALID_NUMBER, true),
            (JSON_ERR_INVALID_STRING, VARIANT_TAG_INVALID_STRING, true),
            (JSON_ERR_INVALID_ESCAPE, VARIANT_TAG_INVALID_ESCAPE, true),
            (JSON_ERR_TRAILING_CHARS, VARIANT_TAG_TRAILING_CHARS, false),
        ];

        for &(err_code, vtag, has_msg) in cases {
            let cond = self.value(
                *block,
                NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(err_code)),
            );
            let hit_b = self.builder.new_block();
            let next_b = self.builder.new_block();
            self.builder.blocks[block.0 as usize].terminator =
                Terminator::Branch(cond, hit_b, next_b);
            let tup = if has_msg {
                self.value(
                    hit_b,
                    NirExpr::NewObject(self.plan.nir_types.tuple_i64_str, vec![offset, msg]),
                )
            } else {
                self.value(
                    hit_b,
                    NirExpr::NewObject(self.plan.nir_types.tuple_i64, vec![offset]),
                )
            };
            let ev = self.value(hit_b, NirExpr::NewEnum(err_ty, vtag, tup));
            self.builder.blocks[hit_b.0 as usize]
                .stmts
                .push(NirStmt::Assign(result, NirExpr::Use(ev)));
            self.builder.blocks[hit_b.0 as usize].terminator = Terminator::Goto(join_b);
            *block = next_b;
        }

        let is_depth = self.value(
            *block,
            NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(JSON_ERR_DEPTH_EXCEEDED)),
        );
        let depth_b = self.builder.new_block();
        let tok_b = self.builder.new_block();
        self.builder.blocks[block.0 as usize].terminator =
            Terminator::Branch(is_depth, depth_b, tok_b);

        self.builder.blocks[depth_b.0 as usize]
            .stmts
            .push(NirStmt::Assign(
                result,
                NirExpr::Use(NirValue::ConstEnum(err_ty, VARIANT_TAG_DEPTH_EXCEEDED)),
            ));
        self.builder.blocks[depth_b.0 as usize].terminator = Terminator::Goto(join_b);

        let tok_tup = self.value(
            tok_b,
            NirExpr::NewObject(self.plan.nir_types.tuple_i64_str, vec![offset, msg]),
        );
        let tok_ev = self.value(
            tok_b,
            NirExpr::NewEnum(err_ty, VARIANT_TAG_UNEXPECTED_TOKEN, tok_tup),
        );
        self.builder.blocks[tok_b.0 as usize]
            .stmts
            .push(NirStmt::Assign(result, NirExpr::Use(tok_ev)));
        self.builder.blocks[tok_b.0 as usize].terminator = Terminator::Goto(join_b);

        *block = join_b;
        NirValue::Local(result)
    }

    fn emit_component(
        &mut self,
        ty: TypeIndex,
        cnode: NirValue,
        cpath: NirValue,
        block: &mut BlockId,
    ) -> NirValue {
        let canon = self
            .resolved
            .type_pool
            .canonical_type(ty)
            .expect("checked Json component type");

        if let Some(&(_, dec_sym, takes_bool)) = self
            .plan
            .custom_decoders
            .iter()
            .find(|&&(t, _, _)| t == canon)
        {
            let raw_tup = self.value(
                *block,
                NirExpr::CallBuiltin(
                    runtime::ids::JSON_TAPE_READ_STR,
                    vec![self.tape, cnode, NirValue::ConstInt(2)],
                ),
            );
            let tup = self.value(
                *block,
                NirExpr::TypeAssert(raw_tup, self.plan.nir_types.tuple_i64_any_str),
            );
            let raw_any = self.value(*block, NirExpr::FieldAccess(tup, 1));
            let raw_json = self.value(
                *block,
                NirExpr::TypeAssert(raw_any, Intrinsic::Str.type_index()),
            );
            let dec_fn = self.builder.func_map[&dec_sym];
            let call_args = if takes_bool {
                vec![raw_json, self.allow_unknown]
            } else {
                vec![raw_json]
            };
            let res = self.value(*block, NirExpr::Call(dec_fn, call_args));
            let is_ok = self.value(*block, NirExpr::ErrorIsOk(res));
            let ok_b = self.builder.new_block();
            let err_b = self.builder.new_block();
            self.builder.blocks[block.0 as usize].terminator =
                Terminator::Branch(is_ok, ok_b, err_b);

            let ep = self.value(err_b, NirExpr::ErrorPayload(res));
            self.emit_return_err(ep, err_b);

            *block = ok_b;
            let payload = self.value(ok_b, NirExpr::ErrorPayload(res));
            return self.value(ok_b, NirExpr::TypeAssert(payload, canon));
        }

        if let Some(&helper_fn) = self.read_helpers.get(&canon) {
            let res = self.value(
                *block,
                NirExpr::Call(helper_fn, vec![self.tape, cnode, cpath, self.allow_unknown]),
            );
            let is_ok = self.value(*block, NirExpr::ErrorIsOk(res));
            let ok_b = self.builder.new_block();
            let err_b = self.builder.new_block();
            self.builder.blocks[block.0 as usize].terminator =
                Terminator::Branch(is_ok, ok_b, err_b);

            let ep = self.value(err_b, NirExpr::ErrorPayload(res));
            self.emit_return_err(ep, err_b);

            *block = ok_b;
            let payload = self.value(ok_b, NirExpr::ErrorPayload(res));
            return self.value(ok_b, NirExpr::TypeAssert(payload, canon));
        }

        if let Some(intr) = self.resolved.type_pool.as_intrinsic(canon) {
            return self.emit_intrinsic_read(intr, canon, cnode, cpath, block);
        }

        match self.resolved.type_pool.get(canon).kind.clone() {
            TypeKind::Optional { inner } => {
                let kind = self.emit_tape_kind(cnode, *block);
                let is_null = self.value(
                    *block,
                    NirExpr::BinOp(BinOp::Eq, kind, NirValue::ConstInt(TAPE_KIND_NULL)),
                );
                if self.resolved.type_pool.as_intrinsic(inner) == Some(Intrinsic::NoReturn) {
                    let ok_b = self.builder.new_block();
                    let mut err_b = self.builder.new_block();
                    self.builder.blocks[block.0 as usize].terminator =
                        Terminator::Branch(is_null, ok_b, err_b);
                    let actual = self.emit_kind_name(kind, &mut err_b);
                    self.emit_type_mismatch(cpath, "null", actual, err_b);
                    *block = ok_b;
                    return NirValue::Null;
                }
                let null_b = self.builder.new_block();
                let mut some_b = self.builder.new_block();
                let join_b = self.builder.new_block();
                let res_local = self.builder.alloc_local();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_null, null_b, some_b);

                self.builder.blocks[null_b.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(res_local, NirExpr::Use(NirValue::Null)));
                self.builder.blocks[null_b.0 as usize].terminator = Terminator::Goto(join_b);

                let inner_val = self.emit_component(inner, cnode, cpath, &mut some_b);
                self.builder.blocks[some_b.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(res_local, NirExpr::Use(inner_val)));
                self.builder.blocks[some_b.0 as usize].terminator = Terminator::Goto(join_b);

                *block = join_b;
                NirValue::Local(res_local)
            }
            TypeKind::Tuple { elements } => {
                self.emit_tuple_read(canon, &elements, cnode, cpath, block)
            }
            _ => unreachable!("Json read prerequisite was validated during resolution"),
        }
    }

    fn emit_intrinsic_read(
        &mut self,
        intr: Intrinsic,
        canon: TypeIndex,
        cnode: NirValue,
        cpath: NirValue,
        block: &mut BlockId,
    ) -> NirValue {
        let int_spec: Option<(i64, &'static str)> = match intr {
            Intrinsic::I8 => Some((0, "i8")),
            Intrinsic::I16 => Some((1, "i16")),
            Intrinsic::I32 => Some((2, "i32")),
            Intrinsic::I64 => Some((3, "i64")),
            Intrinsic::I128 => Some((4, "i128")),
            Intrinsic::Isize => Some((5, "isize")),
            Intrinsic::U8 => Some((6, "u8")),
            Intrinsic::U16 => Some((7, "u16")),
            Intrinsic::U32 => Some((8, "u32")),
            Intrinsic::U64 => Some((9, "u64")),
            Intrinsic::U128 => Some((10, "u128")),
            Intrinsic::Usize => Some((11, "usize")),
            _ => None,
        };
        if let Some((width_tag, type_name)) = int_spec {
            let raw = self.value(
                *block,
                NirExpr::CallBuiltin(
                    runtime::ids::JSON_TAPE_READ_INT,
                    vec![self.tape, cnode, NirValue::ConstInt(width_tag)],
                ),
            );
            let tup = self.value(
                *block,
                NirExpr::TypeAssert(raw, self.plan.nir_types.tuple_i64_any_str),
            );
            let code = self.value(*block, NirExpr::FieldAccess(tup, 0));
            let val_any = self.value(*block, NirExpr::FieldAccess(tup, 1));
            let info = self.value(*block, NirExpr::FieldAccess(tup, 2));

            let is_ok = self.value(
                *block,
                NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(0)),
            );
            let ok_b = self.builder.new_block();
            let err_check_b = self.builder.new_block();
            self.builder.blocks[block.0 as usize].terminator =
                Terminator::Branch(is_ok, ok_b, err_check_b);

            let is_oor = self.value(
                err_check_b,
                NirExpr::BinOp(
                    BinOp::Eq,
                    code,
                    NirValue::ConstInt(JSON_ERR_INT_OUT_OF_RANGE),
                ),
            );
            let oor_b = self.builder.new_block();
            let mis_b = self.builder.new_block();
            self.builder.blocks[err_check_b.0 as usize].terminator =
                Terminator::Branch(is_oor, oor_b, mis_b);

            let info_oor = self.value(
                oor_b,
                NirExpr::TypeAssert(info, Intrinsic::Str.type_index()),
            );
            self.emit_int_out_of_range(cpath, info_oor, type_name, oor_b);

            let info_mis = self.value(
                mis_b,
                NirExpr::TypeAssert(info, Intrinsic::Str.type_index()),
            );
            self.emit_type_mismatch(cpath, type_name, info_mis, mis_b);

            *block = ok_b;
            return self.value(ok_b, NirExpr::TypeAssert(val_any, canon));
        }

        match intr {
            Intrinsic::F32 | Intrinsic::F64 => {
                let (is_f32, type_name) = if intr == Intrinsic::F32 {
                    (true, "f32")
                } else {
                    (false, "f64")
                };
                let raw = self.value(
                    *block,
                    NirExpr::CallBuiltin(
                        runtime::ids::JSON_TAPE_READ_FLOAT,
                        vec![self.tape, cnode, NirValue::ConstBool(is_f32)],
                    ),
                );
                let tup = self.value(
                    *block,
                    NirExpr::TypeAssert(raw, self.plan.nir_types.tuple_i64_any_str),
                );
                let code = self.value(*block, NirExpr::FieldAccess(tup, 0));
                let val_any = self.value(*block, NirExpr::FieldAccess(tup, 1));
                let info = self.value(*block, NirExpr::FieldAccess(tup, 2));

                let is_ok = self.value(
                    *block,
                    NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(0)),
                );
                let ok_b = self.builder.new_block();
                let err_check_b = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_ok, ok_b, err_check_b);

                let is_oor = self.value(
                    err_check_b,
                    NirExpr::BinOp(
                        BinOp::Eq,
                        code,
                        NirValue::ConstInt(JSON_ERR_INT_OUT_OF_RANGE),
                    ),
                );
                let oor_b = self.builder.new_block();
                let mis_b = self.builder.new_block();
                self.builder.blocks[err_check_b.0 as usize].terminator =
                    Terminator::Branch(is_oor, oor_b, mis_b);

                let info_oor = self.value(
                    oor_b,
                    NirExpr::TypeAssert(info, Intrinsic::Str.type_index()),
                );
                self.emit_int_out_of_range(cpath, info_oor, type_name, oor_b);

                let info_mis = self.value(
                    mis_b,
                    NirExpr::TypeAssert(info, Intrinsic::Str.type_index()),
                );
                self.emit_type_mismatch(cpath, type_name, info_mis, mis_b);

                *block = ok_b;
                self.value(ok_b, NirExpr::TypeAssert(val_any, canon))
            }
            Intrinsic::Bool => {
                let raw = self.value(
                    *block,
                    NirExpr::CallBuiltin(runtime::ids::JSON_TAPE_READ_BOOL, vec![self.tape, cnode]),
                );
                let tup = self.value(
                    *block,
                    NirExpr::TypeAssert(raw, self.plan.nir_types.tuple_i64_bool_str),
                );
                let code = self.value(*block, NirExpr::FieldAccess(tup, 0));
                let flag = self.value(*block, NirExpr::FieldAccess(tup, 1));
                let info = self.value(*block, NirExpr::FieldAccess(tup, 2));

                let is_ok = self.value(
                    *block,
                    NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(0)),
                );
                let ok_b = self.builder.new_block();
                let mis_b = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_ok, ok_b, mis_b);

                let info_mis = self.value(
                    mis_b,
                    NirExpr::TypeAssert(info, Intrinsic::Str.type_index()),
                );
                self.emit_type_mismatch(cpath, "bool", info_mis, mis_b);

                *block = ok_b;
                self.value(ok_b, NirExpr::TypeAssert(flag, canon))
            }
            Intrinsic::Str | Intrinsic::Char => {
                let (want_char, type_name) = if intr == Intrinsic::Char {
                    (true, "char")
                } else {
                    (false, "String")
                };
                let raw = self.value(
                    *block,
                    NirExpr::CallBuiltin(
                        runtime::ids::JSON_TAPE_READ_STR,
                        vec![self.tape, cnode, NirValue::ConstBool(want_char)],
                    ),
                );
                let tup = self.value(
                    *block,
                    NirExpr::TypeAssert(raw, self.plan.nir_types.tuple_i64_any_str),
                );
                let code = self.value(*block, NirExpr::FieldAccess(tup, 0));
                let val_any = self.value(*block, NirExpr::FieldAccess(tup, 1));
                let info = self.value(*block, NirExpr::FieldAccess(tup, 2));

                let is_ok = self.value(
                    *block,
                    NirExpr::BinOp(BinOp::Eq, code, NirValue::ConstInt(0)),
                );
                let ok_b = self.builder.new_block();
                let mis_b = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_ok, ok_b, mis_b);

                let info_mis = self.value(
                    mis_b,
                    NirExpr::TypeAssert(info, Intrinsic::Str.type_index()),
                );
                self.emit_type_mismatch(cpath, type_name, info_mis, mis_b);

                *block = ok_b;
                self.value(ok_b, NirExpr::TypeAssert(val_any, canon))
            }
            Intrinsic::Unit => {
                let kind = self.emit_tape_kind(cnode, *block);
                let is_null = self.value(
                    *block,
                    NirExpr::BinOp(BinOp::Eq, kind, NirValue::ConstInt(TAPE_KIND_NULL)),
                );
                let ok_b = self.builder.new_block();
                let mut mis_b = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_null, ok_b, mis_b);
                let actual = self.emit_kind_name(kind, &mut mis_b);
                self.emit_type_mismatch(cpath, "null", actual, mis_b);
                *block = ok_b;
                NirValue::Unit
            }
            _ => unreachable!("unsupported intrinsic in Json derivation was rejected earlier"),
        }
    }

    fn emit_tuple_read(
        &mut self,
        tuple_ty: TypeIndex,
        elements: &[TypeIndex],
        cnode: NirValue,
        cpath: NirValue,
        block: &mut BlockId,
    ) -> NirValue {
        let expected_desc = format!("array of length {}", elements.len());
        let kind = self.emit_tape_kind(cnode, *block);
        let is_arr = self.value(
            *block,
            NirExpr::BinOp(BinOp::Eq, kind, NirValue::ConstInt(TAPE_KIND_ARR)),
        );
        let len_check_b = self.builder.new_block();
        let mut not_arr_b = self.builder.new_block();
        self.builder.blocks[block.0 as usize].terminator =
            Terminator::Branch(is_arr, len_check_b, not_arr_b);

        let actual = self.emit_kind_name(kind, &mut not_arr_b);
        self.emit_type_mismatch(cpath, &expected_desc, actual, not_arr_b);

        let raw_len = self.value(
            len_check_b,
            NirExpr::CallBuiltin(runtime::ids::JSON_TAPE_ARR_LEN, vec![self.tape, cnode]),
        );
        let alen = self.value(
            len_check_b,
            NirExpr::TypeAssert(raw_len, Intrinsic::I64.type_index()),
        );
        let len_ok = self.value(
            len_check_b,
            NirExpr::BinOp(BinOp::Eq, alen, NirValue::ConstInt(elements.len() as i64)),
        );
        let mut elems_b = self.builder.new_block();
        let bad_len_b = self.builder.new_block();
        self.builder.blocks[len_check_b.0 as usize].terminator =
            Terminator::Branch(len_ok, elems_b, bad_len_b);

        self.emit_type_mismatch(
            cpath,
            &expected_desc,
            NirValue::ConstStr(str_interner::intern("array")),
            bad_len_b,
        );

        let mut elem_vals = Vec::with_capacity(elements.len());
        for (idx, &elem_ty) in elements.iter().enumerate() {
            let raw_enode = self.value(
                elems_b,
                NirExpr::CallBuiltin(
                    runtime::ids::JSON_TAPE_ARR_AT,
                    vec![self.tape, cnode, NirValue::ConstInt(idx as i64)],
                ),
            );
            let enode = self.value(
                elems_b,
                NirExpr::TypeAssert(raw_enode, Intrinsic::I64.type_index()),
            );
            let suffix = NirValue::ConstStr(str_interner::intern(&format!("[{idx}]")));
            let epath = self.append_str(elems_b, cpath, suffix);
            let ev = self.emit_component(elem_ty, enode, epath, &mut elems_b);
            elem_vals.push(ev);
        }

        *block = elems_b;
        self.value(elems_b, NirExpr::NewObject(tuple_ty, elem_vals))
    }

    fn is_optional_type(&self, ty: TypeIndex) -> bool {
        self.resolved.type_pool.canonical_type(ty).is_some_and(|c| {
            matches!(
                self.resolved.type_pool.get(c).kind,
                TypeKind::Optional { .. }
            )
        })
    }

    fn emit_aggregate(
        &mut self,
        ty: TypeIndex,
        node: NirValue,
        path: NirValue,
        block: &mut BlockId,
    ) -> NirValue {
        match self.resolved.type_pool.get(ty).kind.clone() {
            TypeKind::Struct { fields, .. } => {
                let kind = self.emit_tape_kind(node, *block);
                let is_obj = self.value(
                    *block,
                    NirExpr::BinOp(BinOp::Eq, kind, NirValue::ConstInt(TAPE_KIND_OBJ)),
                );
                let unk_gate_b = self.builder.new_block();
                let mut not_obj_b = self.builder.new_block();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_obj, unk_gate_b, not_obj_b);

                let actual = self.emit_kind_name(kind, &mut not_obj_b);
                self.emit_type_mismatch(path, "object", actual, not_obj_b);

                let fields_start_b = self.builder.new_block();
                let check_unk_b = self.builder.new_block();
                self.builder.blocks[unk_gate_b.0 as usize].terminator =
                    Terminator::Branch(self.allow_unknown, fields_start_b, check_unk_b);

                let known_keys = if fields.is_empty() {
                    NirValue::Unit
                } else {
                    let kvals = fields.iter().map(|f| NirValue::ConstStr(f.name)).collect();
                    self.value(
                        check_unk_b,
                        NirExpr::NewObject(self.plan.nir_types.struct_known_keys_tuple, kvals),
                    )
                };
                let unk_raw = self.value(
                    check_unk_b,
                    NirExpr::CallBuiltin(
                        runtime::ids::JSON_TAPE_OBJ_UNKNOWN,
                        vec![self.tape, node, known_keys],
                    ),
                );
                let unk_none = self.value(
                    check_unk_b,
                    NirExpr::BinOp(BinOp::Eq, unk_raw, NirValue::Null),
                );
                let unk_err_b = self.builder.new_block();
                self.builder.blocks[check_unk_b.0 as usize].terminator =
                    Terminator::Branch(unk_none, fields_start_b, unk_err_b);

                let unk_str = self.value(
                    unk_err_b,
                    NirExpr::TypeAssert(unk_raw, Intrinsic::Str.type_index()),
                );
                self.emit_unknown_field(path, unk_str, unk_err_b);

                *block = fields_start_b;
                let mut field_vals = Vec::with_capacity(fields.len());
                for (idx, field) in fields.iter().enumerate() {
                    let raw_cnode = self.value(
                        *block,
                        NirExpr::CallBuiltin(
                            runtime::ids::JSON_TAPE_OBJ_GET,
                            vec![self.tape, node, NirValue::ConstStr(field.name)],
                        ),
                    );
                    let cnode = self.value(
                        *block,
                        NirExpr::TypeAssert(raw_cnode, Intrinsic::I64.type_index()),
                    );
                    let is_missing = self.value(
                        *block,
                        NirExpr::BinOp(BinOp::Lt, cnode, NirValue::ConstInt(0)),
                    );
                    let mut miss_b = self.builder.new_block();
                    let mut pres_b = self.builder.new_block();
                    let join_f = self.builder.new_block();
                    let flocal = self.builder.alloc_local();
                    self.builder.blocks[block.0 as usize].terminator =
                        Terminator::Branch(is_missing, miss_b, pres_b);

                    if let Some(&(_, default_expr)) = self
                        .plan
                        .struct_field_defaults
                        .iter()
                        .find(|&&(fidx, _)| fidx == idx)
                    {
                        let dval =
                            lower_expr(self.resolved, default_expr, self.builder, &mut miss_b);
                        let dval = snapshot(dval, self.builder, miss_b);
                        let dval = self.value(miss_b, NirExpr::TypeAssert(dval, field.ty));
                        self.builder.blocks[miss_b.0 as usize]
                            .stmts
                            .push(NirStmt::Assign(flocal, NirExpr::Use(dval)));
                        self.builder.blocks[miss_b.0 as usize].terminator =
                            Terminator::Goto(join_f);
                    } else if self.is_optional_type(field.ty) {
                        self.builder.blocks[miss_b.0 as usize]
                            .stmts
                            .push(NirStmt::Assign(flocal, NirExpr::Use(NirValue::Null)));
                        self.builder.blocks[miss_b.0 as usize].terminator =
                            Terminator::Goto(join_f);
                    } else {
                        self.emit_missing_field(path, field.name, miss_b);
                    }

                    let fname = str_interner::get(field.name);
                    let suffix = NirValue::ConstStr(str_interner::intern(&format!(".{fname}")));
                    let cpath = self.append_str(pres_b, path, suffix);
                    let pval = self.emit_component(field.ty, cnode, cpath, &mut pres_b);
                    self.builder.blocks[pres_b.0 as usize]
                        .stmts
                        .push(NirStmt::Assign(flocal, NirExpr::Use(pval)));
                    self.builder.blocks[pres_b.0 as usize].terminator = Terminator::Goto(join_f);

                    *block = join_f;
                    field_vals.push(NirValue::Local(flocal));
                }

                self.value(*block, NirExpr::NewObject(ty, field_vals))
            }
            TypeKind::Enum { name, variants, .. } => {
                let enum_name = str_interner::get(name);
                let has_unit = variants.iter().any(|v| v.fields.is_empty());
                let has_payload = variants.iter().any(|v| !v.fields.is_empty());
                let expected_top = match (has_unit, has_payload) {
                    (true, false) => "string",
                    (false, true) => "object",
                    _ => "string or single-key object",
                };

                let kind = self.emit_tape_kind(node, *block);
                let is_str = self.value(
                    *block,
                    NirExpr::BinOp(BinOp::Eq, kind, NirValue::ConstInt(TAPE_KIND_STR)),
                );
                let mut str_b = self.builder.new_block();
                let not_str_b = self.builder.new_block();
                let join_b = self.builder.new_block();
                let res_local = self.builder.alloc_local();
                self.builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(is_str, str_b, not_str_b);

                // Branch 1: JSON string -> unit variant.
                let s_raw = self.value(
                    str_b,
                    NirExpr::CallBuiltin(
                        runtime::ids::JSON_TAPE_READ_STR,
                        vec![self.tape, node, NirValue::ConstBool(false)],
                    ),
                );
                let s_tup = self.value(
                    str_b,
                    NirExpr::TypeAssert(s_raw, self.plan.nir_types.tuple_i64_any_str),
                );
                let vname_any = self.value(str_b, NirExpr::FieldAccess(s_tup, 1));
                let vname_str = self.value(
                    str_b,
                    NirExpr::TypeAssert(vname_any, Intrinsic::Str.type_index()),
                );

                for variant in &variants {
                    let cond = self.value(
                        str_b,
                        NirExpr::BinOp(BinOp::Eq, vname_str, NirValue::ConstStr(variant.name)),
                    );
                    let hit_b = self.builder.new_block();
                    let next_b = self.builder.new_block();
                    self.builder.blocks[str_b.0 as usize].terminator =
                        Terminator::Branch(cond, hit_b, next_b);

                    if variant.fields.is_empty() {
                        self.builder.blocks[hit_b.0 as usize]
                            .stmts
                            .push(NirStmt::Assign(
                                res_local,
                                NirExpr::Use(NirValue::ConstEnum(ty, variant.tag)),
                            ));
                        self.builder.blocks[hit_b.0 as usize].terminator = Terminator::Goto(join_b);
                    } else {
                        self.emit_type_mismatch(
                            path,
                            "object",
                            NirValue::ConstStr(str_interner::intern("string")),
                            hit_b,
                        );
                    }
                    str_b = next_b;
                }
                self.emit_invalid_variant(path, &enum_name, vname_str, str_b);

                // Branch 2: non-string -> must be single-key JSON object.
                let is_obj = self.value(
                    not_str_b,
                    NirExpr::BinOp(BinOp::Eq, kind, NirValue::ConstInt(TAPE_KIND_OBJ)),
                );
                let obj_b = self.builder.new_block();
                let mut bad_kind_b = self.builder.new_block();
                self.builder.blocks[not_str_b.0 as usize].terminator =
                    Terminator::Branch(is_obj, obj_b, bad_kind_b);

                let actual = self.emit_kind_name(kind, &mut bad_kind_b);
                self.emit_type_mismatch(path, expected_top, actual, bad_kind_b);

                let sv_raw = self.value(
                    obj_b,
                    NirExpr::CallBuiltin(
                        runtime::ids::JSON_TAPE_OBJ_SINGLE_VARIANT,
                        vec![self.tape, node],
                    ),
                );
                let sv_tup = self.value(
                    obj_b,
                    NirExpr::TypeAssert(sv_raw, self.plan.nir_types.tuple_i64_str),
                );
                let pnode_raw = self.value(obj_b, NirExpr::FieldAccess(sv_tup, 0));
                let pnode = self.value(
                    obj_b,
                    NirExpr::TypeAssert(pnode_raw, Intrinsic::I64.type_index()),
                );
                let ovname_raw = self.value(obj_b, NirExpr::FieldAccess(sv_tup, 1));
                let ovname = self.value(
                    obj_b,
                    NirExpr::TypeAssert(ovname_raw, Intrinsic::Str.type_index()),
                );

                let bad_obj = self.value(
                    obj_b,
                    NirExpr::BinOp(BinOp::Lt, pnode, NirValue::ConstInt(0)),
                );
                let bad_obj_b = self.builder.new_block();
                let mut dispatch_b = self.builder.new_block();
                self.builder.blocks[obj_b.0 as usize].terminator =
                    Terminator::Branch(bad_obj, bad_obj_b, dispatch_b);

                self.emit_type_mismatch(
                    path,
                    "single-key variant object",
                    NirValue::ConstStr(str_interner::intern("object")),
                    bad_obj_b,
                );

                for (vidx, variant) in variants.iter().enumerate() {
                    let cond = self.value(
                        dispatch_b,
                        NirExpr::BinOp(BinOp::Eq, ovname, NirValue::ConstStr(variant.name)),
                    );
                    let mut hit_b = self.builder.new_block();
                    let next_b = self.builder.new_block();
                    self.builder.blocks[dispatch_b.0 as usize].terminator =
                        Terminator::Branch(cond, hit_b, next_b);

                    let vname_s = str_interner::get(variant.name);
                    let vsuffix = NirValue::ConstStr(str_interner::intern(&format!(".{vname_s}")));
                    let vpath = self.append_str(hit_b, path, vsuffix);

                    match variant.fields.len() {
                        0 => {
                            self.emit_type_mismatch(
                                path,
                                "string",
                                NirValue::ConstStr(str_interner::intern("object")),
                                hit_b,
                            );
                        }
                        1 => {
                            let f0 =
                                self.emit_component(variant.fields[0].ty, pnode, vpath, &mut hit_b);
                            let vtup_ty = self.plan.nir_types.enum_variant_tuples[vidx];
                            let vtup = self.value(hit_b, NirExpr::NewObject(vtup_ty, vec![f0]));
                            let ev = self.value(hit_b, NirExpr::NewEnum(ty, variant.tag, vtup));
                            self.builder.blocks[hit_b.0 as usize]
                                .stmts
                                .push(NirStmt::Assign(res_local, NirExpr::Use(ev)));
                            self.builder.blocks[hit_b.0 as usize].terminator =
                                Terminator::Goto(join_b);
                        }
                        arity => {
                            let exp_arr = format!("array of length {arity}");
                            let pkind = self.emit_tape_kind(pnode, hit_b);
                            let is_arr = self.value(
                                hit_b,
                                NirExpr::BinOp(BinOp::Eq, pkind, NirValue::ConstInt(TAPE_KIND_ARR)),
                            );
                            let len_b = self.builder.new_block();
                            let mut not_arr_b = self.builder.new_block();
                            self.builder.blocks[hit_b.0 as usize].terminator =
                                Terminator::Branch(is_arr, len_b, not_arr_b);

                            let pactual = self.emit_kind_name(pkind, &mut not_arr_b);
                            self.emit_type_mismatch(vpath, &exp_arr, pactual, not_arr_b);

                            let alen_raw = self.value(
                                len_b,
                                NirExpr::CallBuiltin(
                                    runtime::ids::JSON_TAPE_ARR_LEN,
                                    vec![self.tape, pnode],
                                ),
                            );
                            let alen = self.value(
                                len_b,
                                NirExpr::TypeAssert(alen_raw, Intrinsic::I64.type_index()),
                            );
                            let len_ok = self.value(
                                len_b,
                                NirExpr::BinOp(BinOp::Eq, alen, NirValue::ConstInt(arity as i64)),
                            );
                            let mut elems_b = self.builder.new_block();
                            let bad_len_b = self.builder.new_block();
                            self.builder.blocks[len_b.0 as usize].terminator =
                                Terminator::Branch(len_ok, elems_b, bad_len_b);

                            self.emit_type_mismatch(
                                vpath,
                                &exp_arr,
                                NirValue::ConstStr(str_interner::intern("array")),
                                bad_len_b,
                            );

                            let mut fvals = Vec::with_capacity(arity);
                            for (fidx, field) in variant.fields.iter().enumerate() {
                                let enode_raw = self.value(
                                    elems_b,
                                    NirExpr::CallBuiltin(
                                        runtime::ids::JSON_TAPE_ARR_AT,
                                        vec![self.tape, pnode, NirValue::ConstInt(fidx as i64)],
                                    ),
                                );
                                let enode = self.value(
                                    elems_b,
                                    NirExpr::TypeAssert(enode_raw, Intrinsic::I64.type_index()),
                                );
                                let esuf =
                                    NirValue::ConstStr(str_interner::intern(&format!("[{fidx}]")));
                                let epath = self.append_str(elems_b, vpath, esuf);
                                let fv = self.emit_component(field.ty, enode, epath, &mut elems_b);
                                fvals.push(fv);
                            }

                            let vtup_ty = self.plan.nir_types.enum_variant_tuples[vidx];
                            let vtup = self.value(elems_b, NirExpr::NewObject(vtup_ty, fvals));
                            let ev = self.value(elems_b, NirExpr::NewEnum(ty, variant.tag, vtup));
                            self.builder.blocks[elems_b.0 as usize]
                                .stmts
                                .push(NirStmt::Assign(res_local, NirExpr::Use(ev)));
                            self.builder.blocks[elems_b.0 as usize].terminator =
                                Terminator::Goto(join_b);
                        }
                    }

                    dispatch_b = next_b;
                }

                self.emit_invalid_variant(path, &enum_name, ovname, dispatch_b);

                *block = join_b;
                NirValue::Local(res_local)
            }
            TypeKind::Newtype { inner, .. } => {
                let inner_val = self.emit_component(inner, node, path, block);
                self.value(*block, NirExpr::NewObject(ty, vec![inner_val]))
            }
            TypeKind::Tuple { elements } => self.emit_tuple_read(ty, &elements, node, path, block),
            _ => unreachable!("Json derive target was validated during resolution"),
        }
    }
}
