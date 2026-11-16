//! WebAssembly binary validation and pre-resolved module representation.

use std::fmt::Write as _;

use wasmparser::{
    BlockType, CompositeInnerType, ConstExpr, ElementItems, ElementKind, ExternalKind, MemArg,
    Operator, Parser, Payload, RefType, TypeRef, ValType, Validator, WasmFeatures,
};

use crate::{
    WasmError, WasmExportDesc, WasmExportKind, WasmImportDesc, WasmImportKind, WasmVal, WasmValType,
};

pub use crate::simd::SimdOp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncSig {
    pub params: Vec<WasmValType>,
    pub results: Vec<WasmValType>,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryPlan {
    pub min_pages: u32,
    pub max_pages: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct TablePlan {
    pub element: WasmValType,
    pub min: u32,
    pub max: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum ConstInit {
    Val(WasmVal),
    GlobalGet(u32),
}

#[derive(Debug, Clone)]
pub struct GlobalPlan {
    pub val_type: WasmValType,
    pub mutable: bool,
    pub init: ConstInit,
}

#[derive(Debug, Clone)]
pub enum DataSegmentKind {
    Passive,
    Active {
        memory_index: u32,
        offset: ConstInit,
    },
}

#[derive(Debug, Clone)]
pub struct DataSegmentPlan {
    pub kind: DataSegmentKind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum ElemSegmentKind {
    Passive,
    Declared,
    Active { table_index: u32, offset: ConstInit },
}

#[derive(Debug, Clone)]
pub struct ElemSegmentPlan {
    pub kind: ElemSegmentKind,
    pub items: Vec<ConstInit>,
}

#[derive(Debug, Clone, Copy)]
pub struct BranchTarget {
    pub target_pc: u32,
    pub arity: u16,
    pub label_depth: u16,
    pub is_loop: bool,
}

#[derive(Debug, Clone)]
pub struct BrTableTargets {
    pub targets: Box<[BranchTarget]>,
    pub default: BranchTarget,
}

#[derive(Debug, Clone, Copy)]
pub struct MemImmediate {
    pub mem: u32,
    pub offset: u64,
}

impl From<MemArg> for MemImmediate {
    fn from(arg: MemArg) -> Self {
        Self {
            mem: arg.memory,
            offset: arg.offset,
        }
    }
}

#[derive(Debug, Clone)]
pub enum WasmOp {
    Unreachable,
    Nop,
    EnterBlock {
        param_arity: u16,
    },
    IfJump {
        param_arity: u16,
        false_pc: u32,
        pop_on_false: bool,
    },
    ElseJump {
        end_after_pc: u32,
    },
    ExitBlock,
    Br(BranchTarget),
    BrIf(BranchTarget),
    BrTable(Box<BrTableTargets>),
    Return,
    Call(u32),
    CallIndirect {
        type_idx: u32,
        table_idx: u32,
    },
    ReturnCall(u32),
    ReturnCallIndirect {
        type_idx: u32,
        table_idx: u32,
    },
    Drop,
    Select,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),

    TableGet(u32),
    TableSet(u32),
    TableSize(u32),
    TableGrow(u32),
    TableFill(u32),
    TableCopy {
        dst_table: u32,
        src_table: u32,
    },
    TableInit {
        elem_idx: u32,
        table_idx: u32,
    },
    ElemDrop(u32),
    RefNull(WasmValType),
    RefIsNull,
    RefFunc(u32),

    MemorySize(u32),
    MemoryGrow(u32),
    MemoryCopy {
        dst_mem: u32,
        src_mem: u32,
    },
    MemoryFill(u32),
    MemoryInit {
        data_idx: u32,
        mem_idx: u32,
    },
    DataDrop(u32),

    I32Load(MemImmediate),
    I64Load(MemImmediate),
    F32Load(MemImmediate),
    F64Load(MemImmediate),
    I32Load8S(MemImmediate),
    I32Load8U(MemImmediate),
    I32Load16S(MemImmediate),
    I32Load16U(MemImmediate),
    I64Load8S(MemImmediate),
    I64Load8U(MemImmediate),
    I64Load16S(MemImmediate),
    I64Load16U(MemImmediate),
    I64Load32S(MemImmediate),
    I64Load32U(MemImmediate),
    I32Store(MemImmediate),
    I64Store(MemImmediate),
    F32Store(MemImmediate),
    F64Store(MemImmediate),
    I32Store8(MemImmediate),
    I32Store16(MemImmediate),
    I64Store8(MemImmediate),
    I64Store16(MemImmediate),
    I64Store32(MemImmediate),

    I32Const(i32),
    I64Const(i64),
    F32Const(u32),
    F64Const(u64),

    I32Eqz,
    I32Eq,
    I32Ne,
    I32LtS,
    I32LtU,
    I32GtS,
    I32GtU,
    I32LeS,
    I32LeU,
    I32GeS,
    I32GeU,

    I64Eqz,
    I64Eq,
    I64Ne,
    I64LtS,
    I64LtU,
    I64GtS,
    I64GtU,
    I64LeS,
    I64LeU,
    I64GeS,
    I64GeU,

    F32Eq,
    F32Ne,
    F32Lt,
    F32Gt,
    F32Le,
    F32Ge,

    F64Eq,
    F64Ne,
    F64Lt,
    F64Gt,
    F64Le,
    F64Ge,

    I32Clz,
    I32Ctz,
    I32Popcnt,
    I32Add,
    I32Sub,
    I32Mul,
    I32DivS,
    I32DivU,
    I32RemS,
    I32RemU,
    I32And,
    I32Or,
    I32Xor,
    I32Shl,
    I32ShrS,
    I32ShrU,
    I32Rotl,
    I32Rotr,
    I32Extend8S,
    I32Extend16S,

    I64Clz,
    I64Ctz,
    I64Popcnt,
    I64Add,
    I64Sub,
    I64Mul,
    I64DivS,
    I64DivU,
    I64RemS,
    I64RemU,
    I64And,
    I64Or,
    I64Xor,
    I64Shl,
    I64ShrS,
    I64ShrU,
    I64Rotl,
    I64Rotr,
    I64Extend8S,
    I64Extend16S,
    I64Extend32S,

    F32Abs,
    F32Neg,
    F32Ceil,
    F32Floor,
    F32Trunc,
    F32Nearest,
    F32Sqrt,
    F32Add,
    F32Sub,
    F32Mul,
    F32Div,
    F32Min,
    F32Max,
    F32Copysign,

    F64Abs,
    F64Neg,
    F64Ceil,
    F64Floor,
    F64Trunc,
    F64Nearest,
    F64Sqrt,
    F64Add,
    F64Sub,
    F64Mul,
    F64Div,
    F64Min,
    F64Max,
    F64Copysign,

    I32WrapI64,
    I32TruncF32S,
    I32TruncF32U,
    I32TruncF64S,
    I32TruncF64U,
    I64ExtendI32S,
    I64ExtendI32U,
    I64TruncF32S,
    I64TruncF32U,
    I64TruncF64S,
    I64TruncF64U,
    F32ConvertI32S,
    F32ConvertI32U,
    F32ConvertI64S,
    F32ConvertI64U,
    F32DemoteF64,
    F64ConvertI32S,
    F64ConvertI32U,
    F64ConvertI64S,
    F64ConvertI64U,
    F64PromoteF32,
    I32ReinterpretF32,
    I64ReinterpretF64,
    F32ReinterpretI32,
    F64ReinterpretI64,

    I32TruncSatF32S,
    I32TruncSatF32U,
    I32TruncSatF64S,
    I32TruncSatF64U,
    I64TruncSatF32S,
    I64TruncSatF32U,
    I64TruncSatF64S,
    I64TruncSatF64U,

    Simd(Box<SimdOp>),
}

#[derive(Debug, Clone)]
pub struct CompiledWasmFunc {
    pub type_idx: u32,
    pub extra_locals: Vec<WasmValType>,
    pub ops: Box<[WasmOp]>,
}

#[derive(Debug, Clone)]
pub enum ExportItem {
    Func(u32),
    Table(u32),
    Memory(u32),
    Global(u32),
}

#[derive(Debug, Clone)]
pub struct ExportRecord {
    pub name: String,
    pub item: ExportItem,
}

#[derive(Debug, Clone)]
pub struct FuncImportPlan {
    pub module: String,
    pub name: String,
    pub type_idx: u32,
}

#[derive(Debug, Clone)]
pub struct MemoryImportPlan {
    pub module: String,
    pub name: String,
    pub plan: MemoryPlan,
}

#[derive(Debug, Clone)]
pub struct TableImportPlan {
    pub module: String,
    pub name: String,
    pub plan: TablePlan,
}

#[derive(Debug, Clone)]
pub struct GlobalImportPlan {
    pub module: String,
    pub name: String,
    pub val_type: WasmValType,
    pub mutable: bool,
}

/// Validated, pre-decoded WebAssembly module ready for fast repeated instantiation.
#[derive(Debug, Clone)]
pub struct WasmModule {
    pub(crate) types: Vec<FuncSig>,
    pub(crate) func_imports: Vec<FuncImportPlan>,
    pub(crate) mem_imports: Vec<MemoryImportPlan>,
    pub(crate) table_imports: Vec<TableImportPlan>,
    pub(crate) global_imports: Vec<GlobalImportPlan>,
    pub(crate) defined_funcs: Vec<CompiledWasmFunc>,
    pub(crate) defined_tables: Vec<TablePlan>,
    pub(crate) defined_memories: Vec<MemoryPlan>,
    pub(crate) defined_globals: Vec<GlobalPlan>,
    pub(crate) exports: Vec<ExportRecord>,
    pub(crate) start_func: Option<u32>,
    pub(crate) elems: Vec<ElemSegmentPlan>,
    pub(crate) data: Vec<DataSegmentPlan>,
    pub(crate) import_order: Vec<WasmImportDesc>,
}

pub(crate) fn supported_features() -> WasmFeatures {
    WasmFeatures::MUTABLE_GLOBAL
        | WasmFeatures::SATURATING_FLOAT_TO_INT
        | WasmFeatures::SIGN_EXTENSION
        | WasmFeatures::MULTI_VALUE
        | WasmFeatures::BULK_MEMORY
        | WasmFeatures::REFERENCE_TYPES
        | WasmFeatures::SIMD
        | WasmFeatures::TAIL_CALL
        | WasmFeatures::MULTI_MEMORY
        | WasmFeatures::FLOATS
}

pub(crate) fn validate_only(bytes: &[u8]) -> Result<(), WasmError> {
    let mut validator = Validator::new_with_features(supported_features());
    validator
        .validate_all(bytes)
        .map(|_| ())
        .map_err(|err| WasmError::Validation(err.to_string()))
}

fn map_val_type(vt: ValType) -> Result<WasmValType, WasmError> {
    match vt {
        ValType::I32 => Ok(WasmValType::I32),
        ValType::I64 => Ok(WasmValType::I64),
        ValType::F32 => Ok(WasmValType::F32),
        ValType::F64 => Ok(WasmValType::F64),
        ValType::V128 => Ok(WasmValType::V128),
        ValType::Ref(rt) => map_ref_type(rt),
    }
}

fn map_ref_type(rt: RefType) -> Result<WasmValType, WasmError> {
    if rt.is_func_ref() {
        Ok(WasmValType::FuncRef)
    } else if rt.is_extern_ref() {
        Ok(WasmValType::ExternRef)
    } else {
        Err(WasmError::UnsupportedFeature(format!(
            "unsupported reference type `{rt}`"
        )))
    }
}

fn parse_const_expr(expr: ConstExpr<'_>) -> Result<ConstInit, WasmError> {
    let mut reader = expr.get_operators_reader();
    let first = reader
        .read()
        .map_err(|e| WasmError::Validation(e.to_string()))?;
    let init = match first {
        Operator::I32Const { value } => ConstInit::Val(WasmVal::I32(value)),
        Operator::I64Const { value } => ConstInit::Val(WasmVal::I64(value)),
        Operator::F32Const { value } => ConstInit::Val(WasmVal::F32(value.bits())),
        Operator::F64Const { value } => ConstInit::Val(WasmVal::F64(value.bits())),
        Operator::V128Const { value } => {
            ConstInit::Val(WasmVal::V128(u128::from_le_bytes(*value.bytes())))
        }
        Operator::RefNull { hty } => {
            let ty = match hty {
                wasmparser::HeapType::Abstract { ty, .. } => match ty {
                    wasmparser::AbstractHeapType::Func => WasmValType::FuncRef,
                    wasmparser::AbstractHeapType::Extern => WasmValType::ExternRef,
                    other => {
                        return Err(WasmError::UnsupportedFeature(format!(
                            "unsupported heap type `{other:?}`"
                        )));
                    }
                },
                other => {
                    return Err(WasmError::UnsupportedFeature(format!(
                        "unsupported concrete heap type `{other:?}`"
                    )));
                }
            };
            ConstInit::Val(WasmVal::default_for(ty))
        }
        Operator::RefFunc { function_index } => {
            ConstInit::Val(WasmVal::FuncRef(Some(function_index)))
        }
        Operator::GlobalGet { global_index } => ConstInit::GlobalGet(global_index),
        other => {
            return Err(WasmError::UnsupportedFeature(format!(
                "unsupported constant initializer `{other:?}`"
            )));
        }
    };
    match reader
        .read()
        .map_err(|e| WasmError::Validation(e.to_string()))?
    {
        Operator::End => Ok(init),
        other => Err(WasmError::UnsupportedFeature(format!(
            "extended constant expressions are not supported (`{other:?}`)"
        ))),
    }
}

impl WasmModule {
    /// Validate and compile a `.wasm` binary into a [`WasmModule`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WasmError> {
        validate_only(bytes)?;

        let mut types = Vec::new();
        let mut func_imports = Vec::new();
        let mut mem_imports = Vec::new();
        let mut table_imports = Vec::new();
        let mut global_imports = Vec::new();
        let mut import_order = Vec::new();
        let mut defined_func_types: Vec<u32> = Vec::new();
        let mut defined_funcs = Vec::new();
        let mut defined_tables = Vec::new();
        let mut defined_memories = Vec::new();
        let mut defined_globals = Vec::new();
        let mut exports = Vec::new();
        let mut start_func = None;
        let mut elems = Vec::new();
        let mut data = Vec::new();
        let mut body_idx = 0usize;

        for payload in Parser::new(0).parse_all(bytes) {
            let payload = payload.map_err(|e| WasmError::Validation(e.to_string()))?;
            match payload {
                Payload::TypeSection(reader) => {
                    for rec_group in reader.into_iter() {
                        let rec_group =
                            rec_group.map_err(|e| WasmError::Validation(e.to_string()))?;
                        for sub_ty in rec_group.into_types() {
                            match sub_ty.composite_type.inner {
                                CompositeInnerType::Func(ft) => {
                                    let params = ft
                                        .params()
                                        .iter()
                                        .copied()
                                        .map(map_val_type)
                                        .collect::<Result<Vec<_>, _>>()?;
                                    let results = ft
                                        .results()
                                        .iter()
                                        .copied()
                                        .map(map_val_type)
                                        .collect::<Result<Vec<_>, _>>()?;
                                    types.push(FuncSig { params, results });
                                }
                                other => {
                                    return Err(WasmError::UnsupportedFeature(format!(
                                        "unsupported composite type `{other:?}`"
                                    )));
                                }
                            }
                        }
                    }
                }
                Payload::ImportSection(reader) => {
                    for imp in reader {
                        let imp = imp.map_err(|e| WasmError::Validation(e.to_string()))?;
                        for (module, name, ty) in expand_imports(imp)? {
                            match ty {
                                TypeRef::Func(type_idx) | TypeRef::FuncExact(type_idx) => {
                                    let sig =
                                        types.get(type_idx as usize).cloned().ok_or_else(|| {
                                            WasmError::Validation("bad func type".into())
                                        })?;
                                    func_imports.push(FuncImportPlan {
                                        module: module.clone(),
                                        name: name.clone(),
                                        type_idx,
                                    });
                                    import_order.push(WasmImportDesc {
                                        module,
                                        name,
                                        kind: WasmImportKind::Func(sig),
                                    });
                                }
                                TypeRef::Memory(mem_ty) => {
                                    if mem_ty.memory64 {
                                        return Err(WasmError::UnsupportedFeature(
                                            "memory64 is not supported".into(),
                                        ));
                                    }
                                    let plan = MemoryPlan {
                                        min_pages: mem_ty.initial as u32,
                                        max_pages: mem_ty.maximum.map(|m| m as u32),
                                    };
                                    mem_imports.push(MemoryImportPlan {
                                        module: module.clone(),
                                        name: name.clone(),
                                        plan,
                                    });
                                    import_order.push(WasmImportDesc {
                                        module,
                                        name,
                                        kind: WasmImportKind::Memory {
                                            min_pages: plan.min_pages,
                                            max_pages: plan.max_pages,
                                        },
                                    });
                                }
                                TypeRef::Table(tab_ty) => {
                                    if tab_ty.table64 {
                                        return Err(WasmError::UnsupportedFeature(
                                            "table64 is not supported".into(),
                                        ));
                                    }
                                    let element = map_ref_type(tab_ty.element_type)?;
                                    let plan = TablePlan {
                                        element,
                                        min: tab_ty.initial as u32,
                                        max: tab_ty.maximum.map(|m| m as u32),
                                    };
                                    table_imports.push(TableImportPlan {
                                        module: module.clone(),
                                        name: name.clone(),
                                        plan,
                                    });
                                    import_order.push(WasmImportDesc {
                                        module,
                                        name,
                                        kind: WasmImportKind::Table {
                                            element,
                                            min: plan.min,
                                            max: plan.max,
                                        },
                                    });
                                }
                                TypeRef::Global(glob_ty) => {
                                    let val_type = map_val_type(glob_ty.content_type)?;
                                    global_imports.push(GlobalImportPlan {
                                        module: module.clone(),
                                        name: name.clone(),
                                        val_type,
                                        mutable: glob_ty.mutable,
                                    });
                                    import_order.push(WasmImportDesc {
                                        module,
                                        name,
                                        kind: WasmImportKind::Global {
                                            val_type,
                                            mutable: glob_ty.mutable,
                                        },
                                    });
                                }
                                TypeRef::Tag(_) => {
                                    return Err(WasmError::UnsupportedFeature(
                                        "wasm exception tags are not supported".into(),
                                    ));
                                }
                            }
                        }
                    }
                }
                Payload::FunctionSection(reader) => {
                    for ty_idx in reader {
                        defined_func_types
                            .push(ty_idx.map_err(|e| WasmError::Validation(e.to_string()))?);
                    }
                }
                Payload::TableSection(reader) => {
                    for tab in reader {
                        let tab = tab.map_err(|e| WasmError::Validation(e.to_string()))?;
                        if tab.ty.table64 {
                            return Err(WasmError::UnsupportedFeature(
                                "table64 is not supported".into(),
                            ));
                        }
                        if !matches!(tab.init, wasmparser::TableInit::RefNull) {
                            return Err(WasmError::UnsupportedFeature(
                                "non-null table initializers are not supported".into(),
                            ));
                        }
                        defined_tables.push(TablePlan {
                            element: map_ref_type(tab.ty.element_type)?,
                            min: tab.ty.initial as u32,
                            max: tab.ty.maximum.map(|m| m as u32),
                        });
                    }
                }
                Payload::MemorySection(reader) => {
                    for mem in reader {
                        let mem = mem.map_err(|e| WasmError::Validation(e.to_string()))?;
                        if mem.memory64 {
                            return Err(WasmError::UnsupportedFeature(
                                "memory64 is not supported".into(),
                            ));
                        }
                        defined_memories.push(MemoryPlan {
                            min_pages: mem.initial as u32,
                            max_pages: mem.maximum.map(|m| m as u32),
                        });
                    }
                }
                Payload::GlobalSection(reader) => {
                    for glob in reader {
                        let glob = glob.map_err(|e| WasmError::Validation(e.to_string()))?;
                        defined_globals.push(GlobalPlan {
                            val_type: map_val_type(glob.ty.content_type)?,
                            mutable: glob.ty.mutable,
                            init: parse_const_expr(glob.init_expr)?,
                        });
                    }
                }
                Payload::ExportSection(reader) => {
                    for exp in reader {
                        let exp = exp.map_err(|e| WasmError::Validation(e.to_string()))?;
                        let item = match exp.kind {
                            ExternalKind::Func | ExternalKind::FuncExact => {
                                ExportItem::Func(exp.index)
                            }
                            ExternalKind::Table => ExportItem::Table(exp.index),
                            ExternalKind::Memory => ExportItem::Memory(exp.index),
                            ExternalKind::Global => ExportItem::Global(exp.index),
                            ExternalKind::Tag => {
                                return Err(WasmError::UnsupportedFeature(
                                    "wasm exception tags are not supported".into(),
                                ));
                            }
                        };
                        exports.push(ExportRecord {
                            name: exp.name.to_owned(),
                            item,
                        });
                    }
                }
                Payload::StartSection { func, .. } => {
                    start_func = Some(func);
                }
                Payload::ElementSection(reader) => {
                    for elem in reader {
                        let elem = elem.map_err(|e| WasmError::Validation(e.to_string()))?;
                        let kind = match elem.kind {
                            ElementKind::Passive => ElemSegmentKind::Passive,
                            ElementKind::Declared => ElemSegmentKind::Declared,
                            ElementKind::Active {
                                table_index,
                                offset_expr,
                            } => ElemSegmentKind::Active {
                                table_index: table_index.unwrap_or(0),
                                offset: parse_const_expr(offset_expr)?,
                            },
                        };
                        let items = match elem.items {
                            ElementItems::Functions(funcs) => {
                                let mut out = Vec::new();
                                for f in funcs {
                                    let idx =
                                        f.map_err(|e| WasmError::Validation(e.to_string()))?;
                                    out.push(ConstInit::Val(WasmVal::FuncRef(Some(idx))));
                                }
                                out
                            }
                            ElementItems::Expressions(rt, exprs) => {
                                let _ = map_ref_type(rt)?;
                                let mut out = Vec::new();
                                for ex in exprs {
                                    let ex =
                                        ex.map_err(|e| WasmError::Validation(e.to_string()))?;
                                    out.push(parse_const_expr(ex)?);
                                }
                                out
                            }
                        };
                        elems.push(ElemSegmentPlan { kind, items });
                    }
                }
                Payload::CodeSectionEntry(body) => {
                    let type_idx = *defined_func_types
                        .get(body_idx)
                        .ok_or_else(|| WasmError::Validation("extra code body".into()))?;
                    body_idx += 1;
                    let compiled = compile_func_body(type_idx, &types, body)?;
                    defined_funcs.push(compiled);
                }
                Payload::DataSection(reader) => {
                    for seg in reader {
                        let seg = seg.map_err(|e| WasmError::Validation(e.to_string()))?;
                        let kind = match seg.kind {
                            wasmparser::DataKind::Passive => DataSegmentKind::Passive,
                            wasmparser::DataKind::Active {
                                memory_index,
                                offset_expr,
                            } => DataSegmentKind::Active {
                                memory_index,
                                offset: parse_const_expr(offset_expr)?,
                            },
                        };
                        data.push(DataSegmentPlan {
                            kind,
                            bytes: seg.data.to_vec(),
                        });
                    }
                }
                Payload::ComponentSection { .. } | Payload::ModuleSection { .. } => {
                    return Err(WasmError::UnsupportedFeature(
                        "wasm component model containers are not supported directly; pass a core module".into(),
                    ));
                }
                _ => {}
            }
        }

        Ok(Self {
            types,
            func_imports,
            mem_imports,
            table_imports,
            global_imports,
            defined_funcs,
            defined_tables,
            defined_memories,
            defined_globals,
            exports,
            start_func,
            elems,
            data,
            import_order,
        })
    }

    pub fn imports(&self) -> &[WasmImportDesc] {
        &self.import_order
    }

    pub fn exports(&self) -> Vec<WasmExportDesc> {
        let mut out = Vec::with_capacity(self.exports.len());
        for exp in &self.exports {
            let kind = match exp.item {
                ExportItem::Func(idx) => WasmExportKind::Func(self.func_sig(idx).clone()),
                ExportItem::Memory(idx) => {
                    let plan = self.memory_plan(idx);
                    WasmExportKind::Memory {
                        min_pages: plan.min_pages,
                        max_pages: plan.max_pages,
                    }
                }
                ExportItem::Table(idx) => {
                    let plan = self.table_plan(idx);
                    WasmExportKind::Table {
                        element: plan.element,
                        min: plan.min,
                        max: plan.max,
                    }
                }
                ExportItem::Global(idx) => {
                    let (val_type, mutable) = self.global_desc(idx);
                    WasmExportKind::Global { val_type, mutable }
                }
            };
            out.push(WasmExportDesc {
                name: exp.name.clone(),
                kind,
            });
        }
        out
    }

    pub fn export_func_sig(&self, export_name: &str) -> Option<&FuncSig> {
        let rec = self.exports.iter().find(|e| e.name == export_name)?;
        match rec.item {
            ExportItem::Func(idx) => Some(self.func_sig(idx)),
            _ => None,
        }
    }

    pub(crate) fn func_type_idx(&self, func_idx: u32) -> u32 {
        let i = func_idx as usize;
        if i < self.func_imports.len() {
            self.func_imports[i].type_idx
        } else {
            self.defined_funcs[i - self.func_imports.len()].type_idx
        }
    }

    pub(crate) fn func_sig(&self, func_idx: u32) -> &FuncSig {
        &self.types[self.func_type_idx(func_idx) as usize]
    }

    pub(crate) fn memory_plan(&self, mem_idx: u32) -> MemoryPlan {
        let i = mem_idx as usize;
        if i < self.mem_imports.len() {
            self.mem_imports[i].plan
        } else {
            self.defined_memories[i - self.mem_imports.len()]
        }
    }

    pub(crate) fn table_plan(&self, table_idx: u32) -> TablePlan {
        let i = table_idx as usize;
        if i < self.table_imports.len() {
            self.table_imports[i].plan
        } else {
            self.defined_tables[i - self.table_imports.len()]
        }
    }

    pub(crate) fn global_desc(&self, global_idx: u32) -> (WasmValType, bool) {
        let i = global_idx as usize;
        if i < self.global_imports.len() {
            let g = &self.global_imports[i];
            (g.val_type, g.mutable)
        } else {
            let g = &self.defined_globals[i - self.global_imports.len()];
            (g.val_type, g.mutable)
        }
    }

    /// Render a concise human-readable module inspection report for `nessa disasm`.
    pub fn describe_summary(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "=== WebAssembly Module ===");
        let _ = writeln!(out, "types: {}", self.types.len());
        let _ = writeln!(
            out,
            "functions: {} imported + {} defined",
            self.func_imports.len(),
            self.defined_funcs.len()
        );
        let _ = writeln!(
            out,
            "memories: {} imported + {} defined",
            self.mem_imports.len(),
            self.defined_memories.len()
        );
        let _ = writeln!(
            out,
            "tables: {} imported + {} defined",
            self.table_imports.len(),
            self.defined_tables.len()
        );
        let _ = writeln!(
            out,
            "globals: {} imported + {} defined",
            self.global_imports.len(),
            self.defined_globals.len()
        );
        if let Some(start) = self.start_func {
            let _ = writeln!(out, "start: func[{start}]");
        }
        if !self.import_order.is_empty() {
            let _ = writeln!(out, "\n[Imports ({})]", self.import_order.len());
            for imp in &self.import_order {
                let _ = writeln!(
                    out,
                    "  {}::{} : {}",
                    imp.module,
                    imp.name,
                    format_import_kind(&imp.kind)
                );
            }
        }
        let exps = self.exports();
        if !exps.is_empty() {
            let _ = writeln!(out, "\n[Exports ({})]", exps.len());
            for exp in &exps {
                let _ = writeln!(out, "  {} : {}", exp.name, format_export_kind(&exp.kind));
            }
        }
        out
    }
}

fn format_sig(sig: &FuncSig) -> String {
    let params = sig
        .params
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let results = sig
        .results
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("({params}) -> ({results})")
}

fn format_import_kind(kind: &WasmImportKind) -> String {
    match kind {
        WasmImportKind::Func(sig) => format!("func {}", format_sig(sig)),
        WasmImportKind::Memory {
            min_pages,
            max_pages,
        } => format!("memory (min={min_pages}, max={max_pages:?})"),
        WasmImportKind::Table { element, min, max } => {
            format!("table {element} (min={min}, max={max:?})")
        }
        WasmImportKind::Global { val_type, mutable } => {
            format!("global {}{val_type}", if *mutable { "mut " } else { "" })
        }
    }
}

fn format_export_kind(kind: &WasmExportKind) -> String {
    match kind {
        WasmExportKind::Func(sig) => format!("func {}", format_sig(sig)),
        WasmExportKind::Memory {
            min_pages,
            max_pages,
        } => format!("memory (min={min_pages}, max={max_pages:?})"),
        WasmExportKind::Table { element, min, max } => {
            format!("table {element} (min={min}, max={max:?})")
        }
        WasmExportKind::Global { val_type, mutable } => {
            format!("global {}{val_type}", if *mutable { "mut " } else { "" })
        }
    }
}

fn expand_imports(
    imp: wasmparser::Imports<'_>,
) -> Result<Vec<(String, String, TypeRef)>, WasmError> {
    match imp {
        wasmparser::Imports::Single(_, single) => Ok(vec![(
            single.module.to_owned(),
            single.name.to_owned(),
            single.ty,
        )]),
        wasmparser::Imports::Compact1 { module, items } => {
            let mut out = Vec::new();
            for item in items {
                let item = item.map_err(|e| WasmError::Validation(e.to_string()))?;
                out.push((module.to_owned(), item.name.to_owned(), item.ty));
            }
            Ok(out)
        }
        wasmparser::Imports::Compact2 { module, ty, names } => {
            let mut out = Vec::new();
            for name in names {
                let name = name.map_err(|e| WasmError::Validation(e.to_string()))?;
                out.push((module.to_owned(), name.to_owned(), ty));
            }
            Ok(out)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CtrlKind {
    Function,
    Block,
    Loop {
        body_pc: u32,
    },
    If {
        if_op_idx: usize,
        else_op_idx: Option<usize>,
    },
}

struct CtrlFrame {
    kind: CtrlKind,
    param_arity: u16,
    result_arity: u16,
    pending_patches: Vec<PatchSlot>,
}

#[derive(Clone, Copy)]
enum PatchSlot {
    Br(usize),
    BrTableTarget(usize, usize),
    BrTableDefault(usize),
}

fn block_arity(block_ty: BlockType, types: &[FuncSig]) -> Result<(u16, u16), WasmError> {
    match block_ty {
        BlockType::Empty => Ok((0, 0)),
        BlockType::Type(_) => Ok((0, 1)),
        BlockType::FuncType(idx) => {
            let sig = types
                .get(idx as usize)
                .ok_or_else(|| WasmError::Validation("invalid block type index".into()))?;
            let p = u16::try_from(sig.params.len())
                .map_err(|_| WasmError::UnsupportedFeature("block param arity > 65535".into()))?;
            let r = u16::try_from(sig.results.len())
                .map_err(|_| WasmError::UnsupportedFeature("block result arity > 65535".into()))?;
            Ok((p, r))
        }
    }
}

fn compile_func_body(
    type_idx: u32,
    types: &[FuncSig],
    body: wasmparser::FunctionBody<'_>,
) -> Result<CompiledWasmFunc, WasmError> {
    let sig = types
        .get(type_idx as usize)
        .ok_or_else(|| WasmError::Validation("invalid function type index".into()))?;
    let mut extra_locals = Vec::new();
    let locals_reader = body
        .get_locals_reader()
        .map_err(|e| WasmError::Validation(e.to_string()))?;
    for decl in locals_reader {
        let (count, vt) = decl.map_err(|e| WasmError::Validation(e.to_string()))?;
        let mapped = map_val_type(vt)?;
        let new_len = extra_locals
            .len()
            .checked_add(count as usize)
            .filter(|&n| n <= 65_536)
            .ok_or_else(|| WasmError::UnsupportedFeature("too many local variables".into()))?;
        extra_locals.resize(new_len, mapped);
    }

    let func_res_arity = u16::try_from(sig.results.len())
        .map_err(|_| WasmError::UnsupportedFeature("function result arity > 65535".into()))?;

    let mut ops: Vec<WasmOp> = Vec::new();
    let mut ctrl: Vec<CtrlFrame> = vec![CtrlFrame {
        kind: CtrlKind::Function,
        param_arity: 0,
        result_arity: func_res_arity,
        pending_patches: Vec::new(),
    }];

    let make_branch_target = |ctrl_stack: &mut [CtrlFrame],
                              depth: u32,
                              patch: PatchSlot|
     -> Result<BranchTarget, WasmError> {
        let len = ctrl_stack.len();
        let idx = len
            .checked_sub(1 + depth as usize)
            .ok_or_else(|| WasmError::Validation("invalid branch depth".into()))?;
        let depth_u16 = u16::try_from(depth)
            .map_err(|_| WasmError::UnsupportedFeature("control nesting > 65535".into()))?;
        let frame = &mut ctrl_stack[idx];
        match frame.kind {
            CtrlKind::Loop { body_pc } => Ok(BranchTarget {
                target_pc: body_pc,
                arity: frame.param_arity,
                label_depth: depth_u16,
                is_loop: true,
            }),
            CtrlKind::Function | CtrlKind::Block | CtrlKind::If { .. } => {
                frame.pending_patches.push(patch);
                Ok(BranchTarget {
                    target_pc: u32::MAX,
                    arity: frame.result_arity,
                    label_depth: depth_u16,
                    is_loop: false,
                })
            }
        }
    };

    let mut reader = body
        .get_operators_reader()
        .map_err(|e| WasmError::Validation(e.to_string()))?;

    while !reader.eof() {
        let op = reader
            .read()
            .map_err(|e| WasmError::Validation(e.to_string()))?;
        match op {
            Operator::Unreachable => ops.push(WasmOp::Unreachable),
            Operator::Nop => ops.push(WasmOp::Nop),
            Operator::Block { blockty } => {
                let (param_arity, result_arity) = block_arity(blockty, types)?;
                ops.push(WasmOp::EnterBlock { param_arity });
                ctrl.push(CtrlFrame {
                    kind: CtrlKind::Block,
                    param_arity,
                    result_arity,
                    pending_patches: Vec::new(),
                });
            }
            Operator::Loop { blockty } => {
                let (param_arity, result_arity) = block_arity(blockty, types)?;
                ops.push(WasmOp::EnterBlock { param_arity });
                let body_pc = ops.len() as u32;
                ctrl.push(CtrlFrame {
                    kind: CtrlKind::Loop { body_pc },
                    param_arity,
                    result_arity,
                    pending_patches: Vec::new(),
                });
            }
            Operator::If { blockty } => {
                let (param_arity, result_arity) = block_arity(blockty, types)?;
                let if_op_idx = ops.len();
                ops.push(WasmOp::IfJump {
                    param_arity,
                    false_pc: u32::MAX,
                    pop_on_false: true,
                });
                ctrl.push(CtrlFrame {
                    kind: CtrlKind::If {
                        if_op_idx,
                        else_op_idx: None,
                    },
                    param_arity,
                    result_arity,
                    pending_patches: Vec::new(),
                });
            }
            Operator::Else => {
                let else_op_idx = ops.len();
                ops.push(WasmOp::ElseJump {
                    end_after_pc: u32::MAX,
                });
                let else_body_pc = ops.len() as u32;
                let Some(top) = ctrl.last_mut() else {
                    return Err(WasmError::Validation("unmatched else".into()));
                };
                match &mut top.kind {
                    CtrlKind::If {
                        if_op_idx,
                        else_op_idx: slot,
                    } => {
                        let idx = *if_op_idx;
                        *slot = Some(else_op_idx);
                        if let WasmOp::IfJump {
                            false_pc,
                            pop_on_false,
                            ..
                        } = &mut ops[idx]
                        {
                            *false_pc = else_body_pc;
                            *pop_on_false = false;
                        }
                    }
                    _ => return Err(WasmError::Validation("else outside if".into())),
                }
            }
            Operator::End => {
                let Some(frame) = ctrl.pop() else {
                    return Err(WasmError::Validation("unmatched end".into()));
                };
                let target_after_end = match frame.kind {
                    CtrlKind::Function => {
                        let ret_pc = ops.len() as u32;
                        ops.push(WasmOp::Return);
                        ret_pc
                    }
                    CtrlKind::Block | CtrlKind::Loop { .. } => {
                        ops.push(WasmOp::ExitBlock);
                        ops.len() as u32
                    }
                    CtrlKind::If {
                        if_op_idx,
                        else_op_idx,
                    } => {
                        ops.push(WasmOp::ExitBlock);
                        let after_pc = ops.len() as u32;
                        if let Some(e_idx) = else_op_idx {
                            if let WasmOp::ElseJump { end_after_pc } = &mut ops[e_idx] {
                                *end_after_pc = after_pc;
                            }
                        } else if let WasmOp::IfJump {
                            false_pc,
                            pop_on_false,
                            ..
                        } = &mut ops[if_op_idx]
                        {
                            *false_pc = after_pc;
                            *pop_on_false = true;
                        }
                        after_pc
                    }
                };
                for patch in frame.pending_patches {
                    match patch {
                        PatchSlot::Br(op_idx) => match &mut ops[op_idx] {
                            WasmOp::Br(bt) | WasmOp::BrIf(bt) => bt.target_pc = target_after_end,
                            _ => unreachable!(),
                        },
                        PatchSlot::BrTableTarget(op_idx, arm_idx) => {
                            if let WasmOp::BrTable(tbl) = &mut ops[op_idx] {
                                tbl.targets[arm_idx].target_pc = target_after_end;
                            }
                        }
                        PatchSlot::BrTableDefault(op_idx) => {
                            if let WasmOp::BrTable(tbl) = &mut ops[op_idx] {
                                tbl.default.target_pc = target_after_end;
                            }
                        }
                    }
                }
            }
            Operator::Br { relative_depth } => {
                let op_idx = ops.len();
                let bt = make_branch_target(&mut ctrl, relative_depth, PatchSlot::Br(op_idx))?;
                ops.push(WasmOp::Br(bt));
            }
            Operator::BrIf { relative_depth } => {
                let op_idx = ops.len();
                let bt = make_branch_target(&mut ctrl, relative_depth, PatchSlot::Br(op_idx))?;
                ops.push(WasmOp::BrIf(bt));
            }
            Operator::BrTable { targets } => {
                let op_idx = ops.len();
                let mut vec_targets = Vec::with_capacity(targets.len() as usize);
                for (arm_i, depth_res) in targets.targets().enumerate() {
                    let depth = depth_res.map_err(|e| WasmError::Validation(e.to_string()))?;
                    vec_targets.push(make_branch_target(
                        &mut ctrl,
                        depth,
                        PatchSlot::BrTableTarget(op_idx, arm_i),
                    )?);
                }
                let default = make_branch_target(
                    &mut ctrl,
                    targets.default(),
                    PatchSlot::BrTableDefault(op_idx),
                )?;
                ops.push(WasmOp::BrTable(Box::new(BrTableTargets {
                    targets: vec_targets.into_boxed_slice(),
                    default,
                })));
            }
            Operator::Return => ops.push(WasmOp::Return),
            Operator::Call { function_index } => ops.push(WasmOp::Call(function_index)),
            Operator::CallIndirect {
                type_index,
                table_index,
            } => ops.push(WasmOp::CallIndirect {
                type_idx: type_index,
                table_idx: table_index,
            }),
            Operator::ReturnCall { function_index } => ops.push(WasmOp::ReturnCall(function_index)),
            Operator::ReturnCallIndirect {
                type_index,
                table_index,
            } => ops.push(WasmOp::ReturnCallIndirect {
                type_idx: type_index,
                table_idx: table_index,
            }),
            Operator::Drop => ops.push(WasmOp::Drop),
            Operator::Select | Operator::TypedSelect { .. } => ops.push(WasmOp::Select),
            Operator::LocalGet { local_index } => ops.push(WasmOp::LocalGet(local_index)),
            Operator::LocalSet { local_index } => ops.push(WasmOp::LocalSet(local_index)),
            Operator::LocalTee { local_index } => ops.push(WasmOp::LocalTee(local_index)),
            Operator::GlobalGet { global_index } => ops.push(WasmOp::GlobalGet(global_index)),
            Operator::GlobalSet { global_index } => ops.push(WasmOp::GlobalSet(global_index)),

            Operator::TableGet { table } => ops.push(WasmOp::TableGet(table)),
            Operator::TableSet { table } => ops.push(WasmOp::TableSet(table)),
            Operator::TableSize { table } => ops.push(WasmOp::TableSize(table)),
            Operator::TableGrow { table } => ops.push(WasmOp::TableGrow(table)),
            Operator::TableFill { table } => ops.push(WasmOp::TableFill(table)),
            Operator::TableCopy {
                dst_table,
                src_table,
            } => ops.push(WasmOp::TableCopy {
                dst_table,
                src_table,
            }),
            Operator::TableInit { elem_index, table } => ops.push(WasmOp::TableInit {
                elem_idx: elem_index,
                table_idx: table,
            }),
            Operator::ElemDrop { elem_index } => ops.push(WasmOp::ElemDrop(elem_index)),
            Operator::RefNull { hty } => {
                let ty = match hty {
                    wasmparser::HeapType::Abstract { ty, .. } => match ty {
                        wasmparser::AbstractHeapType::Func => WasmValType::FuncRef,
                        wasmparser::AbstractHeapType::Extern => WasmValType::ExternRef,
                        other => {
                            return Err(WasmError::UnsupportedFeature(format!(
                                "unsupported ref.null heap type `{other:?}`"
                            )));
                        }
                    },
                    other => {
                        return Err(WasmError::UnsupportedFeature(format!(
                            "unsupported ref.null heap type `{other:?}`"
                        )));
                    }
                };
                ops.push(WasmOp::RefNull(ty));
            }
            Operator::RefIsNull => ops.push(WasmOp::RefIsNull),
            Operator::RefFunc { function_index } => ops.push(WasmOp::RefFunc(function_index)),

            Operator::MemorySize { mem } => ops.push(WasmOp::MemorySize(mem)),
            Operator::MemoryGrow { mem } => ops.push(WasmOp::MemoryGrow(mem)),
            Operator::MemoryCopy { dst_mem, src_mem } => {
                ops.push(WasmOp::MemoryCopy { dst_mem, src_mem })
            }
            Operator::MemoryFill { mem } => ops.push(WasmOp::MemoryFill(mem)),
            Operator::MemoryInit { data_index, mem } => ops.push(WasmOp::MemoryInit {
                data_idx: data_index,
                mem_idx: mem,
            }),
            Operator::DataDrop { data_index } => ops.push(WasmOp::DataDrop(data_index)),

            Operator::I32Load { memarg } => ops.push(WasmOp::I32Load(memarg.into())),
            Operator::I64Load { memarg } => ops.push(WasmOp::I64Load(memarg.into())),
            Operator::F32Load { memarg } => ops.push(WasmOp::F32Load(memarg.into())),
            Operator::F64Load { memarg } => ops.push(WasmOp::F64Load(memarg.into())),
            Operator::I32Load8S { memarg } => ops.push(WasmOp::I32Load8S(memarg.into())),
            Operator::I32Load8U { memarg } => ops.push(WasmOp::I32Load8U(memarg.into())),
            Operator::I32Load16S { memarg } => ops.push(WasmOp::I32Load16S(memarg.into())),
            Operator::I32Load16U { memarg } => ops.push(WasmOp::I32Load16U(memarg.into())),
            Operator::I64Load8S { memarg } => ops.push(WasmOp::I64Load8S(memarg.into())),
            Operator::I64Load8U { memarg } => ops.push(WasmOp::I64Load8U(memarg.into())),
            Operator::I64Load16S { memarg } => ops.push(WasmOp::I64Load16S(memarg.into())),
            Operator::I64Load16U { memarg } => ops.push(WasmOp::I64Load16U(memarg.into())),
            Operator::I64Load32S { memarg } => ops.push(WasmOp::I64Load32S(memarg.into())),
            Operator::I64Load32U { memarg } => ops.push(WasmOp::I64Load32U(memarg.into())),
            Operator::I32Store { memarg } => ops.push(WasmOp::I32Store(memarg.into())),
            Operator::I64Store { memarg } => ops.push(WasmOp::I64Store(memarg.into())),
            Operator::F32Store { memarg } => ops.push(WasmOp::F32Store(memarg.into())),
            Operator::F64Store { memarg } => ops.push(WasmOp::F64Store(memarg.into())),
            Operator::I32Store8 { memarg } => ops.push(WasmOp::I32Store8(memarg.into())),
            Operator::I32Store16 { memarg } => ops.push(WasmOp::I32Store16(memarg.into())),
            Operator::I64Store8 { memarg } => ops.push(WasmOp::I64Store8(memarg.into())),
            Operator::I64Store16 { memarg } => ops.push(WasmOp::I64Store16(memarg.into())),
            Operator::I64Store32 { memarg } => ops.push(WasmOp::I64Store32(memarg.into())),

            Operator::I32Const { value } => ops.push(WasmOp::I32Const(value)),
            Operator::I64Const { value } => ops.push(WasmOp::I64Const(value)),
            Operator::F32Const { value } => ops.push(WasmOp::F32Const(value.bits())),
            Operator::F64Const { value } => ops.push(WasmOp::F64Const(value.bits())),

            Operator::I32Eqz => ops.push(WasmOp::I32Eqz),
            Operator::I32Eq => ops.push(WasmOp::I32Eq),
            Operator::I32Ne => ops.push(WasmOp::I32Ne),
            Operator::I32LtS => ops.push(WasmOp::I32LtS),
            Operator::I32LtU => ops.push(WasmOp::I32LtU),
            Operator::I32GtS => ops.push(WasmOp::I32GtS),
            Operator::I32GtU => ops.push(WasmOp::I32GtU),
            Operator::I32LeS => ops.push(WasmOp::I32LeS),
            Operator::I32LeU => ops.push(WasmOp::I32LeU),
            Operator::I32GeS => ops.push(WasmOp::I32GeS),
            Operator::I32GeU => ops.push(WasmOp::I32GeU),

            Operator::I64Eqz => ops.push(WasmOp::I64Eqz),
            Operator::I64Eq => ops.push(WasmOp::I64Eq),
            Operator::I64Ne => ops.push(WasmOp::I64Ne),
            Operator::I64LtS => ops.push(WasmOp::I64LtS),
            Operator::I64LtU => ops.push(WasmOp::I64LtU),
            Operator::I64GtS => ops.push(WasmOp::I64GtS),
            Operator::I64GtU => ops.push(WasmOp::I64GtU),
            Operator::I64LeS => ops.push(WasmOp::I64LeS),
            Operator::I64LeU => ops.push(WasmOp::I64LeU),
            Operator::I64GeS => ops.push(WasmOp::I64GeS),
            Operator::I64GeU => ops.push(WasmOp::I64GeU),

            Operator::F32Eq => ops.push(WasmOp::F32Eq),
            Operator::F32Ne => ops.push(WasmOp::F32Ne),
            Operator::F32Lt => ops.push(WasmOp::F32Lt),
            Operator::F32Gt => ops.push(WasmOp::F32Gt),
            Operator::F32Le => ops.push(WasmOp::F32Le),
            Operator::F32Ge => ops.push(WasmOp::F32Ge),

            Operator::F64Eq => ops.push(WasmOp::F64Eq),
            Operator::F64Ne => ops.push(WasmOp::F64Ne),
            Operator::F64Lt => ops.push(WasmOp::F64Lt),
            Operator::F64Gt => ops.push(WasmOp::F64Gt),
            Operator::F64Le => ops.push(WasmOp::F64Le),
            Operator::F64Ge => ops.push(WasmOp::F64Ge),

            Operator::I32Clz => ops.push(WasmOp::I32Clz),
            Operator::I32Ctz => ops.push(WasmOp::I32Ctz),
            Operator::I32Popcnt => ops.push(WasmOp::I32Popcnt),
            Operator::I32Add => ops.push(WasmOp::I32Add),
            Operator::I32Sub => ops.push(WasmOp::I32Sub),
            Operator::I32Mul => ops.push(WasmOp::I32Mul),
            Operator::I32DivS => ops.push(WasmOp::I32DivS),
            Operator::I32DivU => ops.push(WasmOp::I32DivU),
            Operator::I32RemS => ops.push(WasmOp::I32RemS),
            Operator::I32RemU => ops.push(WasmOp::I32RemU),
            Operator::I32And => ops.push(WasmOp::I32And),
            Operator::I32Or => ops.push(WasmOp::I32Or),
            Operator::I32Xor => ops.push(WasmOp::I32Xor),
            Operator::I32Shl => ops.push(WasmOp::I32Shl),
            Operator::I32ShrS => ops.push(WasmOp::I32ShrS),
            Operator::I32ShrU => ops.push(WasmOp::I32ShrU),
            Operator::I32Rotl => ops.push(WasmOp::I32Rotl),
            Operator::I32Rotr => ops.push(WasmOp::I32Rotr),
            Operator::I32Extend8S => ops.push(WasmOp::I32Extend8S),
            Operator::I32Extend16S => ops.push(WasmOp::I32Extend16S),

            Operator::I64Clz => ops.push(WasmOp::I64Clz),
            Operator::I64Ctz => ops.push(WasmOp::I64Ctz),
            Operator::I64Popcnt => ops.push(WasmOp::I64Popcnt),
            Operator::I64Add => ops.push(WasmOp::I64Add),
            Operator::I64Sub => ops.push(WasmOp::I64Sub),
            Operator::I64Mul => ops.push(WasmOp::I64Mul),
            Operator::I64DivS => ops.push(WasmOp::I64DivS),
            Operator::I64DivU => ops.push(WasmOp::I64DivU),
            Operator::I64RemS => ops.push(WasmOp::I64RemS),
            Operator::I64RemU => ops.push(WasmOp::I64RemU),
            Operator::I64And => ops.push(WasmOp::I64And),
            Operator::I64Or => ops.push(WasmOp::I64Or),
            Operator::I64Xor => ops.push(WasmOp::I64Xor),
            Operator::I64Shl => ops.push(WasmOp::I64Shl),
            Operator::I64ShrS => ops.push(WasmOp::I64ShrS),
            Operator::I64ShrU => ops.push(WasmOp::I64ShrU),
            Operator::I64Rotl => ops.push(WasmOp::I64Rotl),
            Operator::I64Rotr => ops.push(WasmOp::I64Rotr),
            Operator::I64Extend8S => ops.push(WasmOp::I64Extend8S),
            Operator::I64Extend16S => ops.push(WasmOp::I64Extend16S),
            Operator::I64Extend32S => ops.push(WasmOp::I64Extend32S),

            Operator::F32Abs => ops.push(WasmOp::F32Abs),
            Operator::F32Neg => ops.push(WasmOp::F32Neg),
            Operator::F32Ceil => ops.push(WasmOp::F32Ceil),
            Operator::F32Floor => ops.push(WasmOp::F32Floor),
            Operator::F32Trunc => ops.push(WasmOp::F32Trunc),
            Operator::F32Nearest => ops.push(WasmOp::F32Nearest),
            Operator::F32Sqrt => ops.push(WasmOp::F32Sqrt),
            Operator::F32Add => ops.push(WasmOp::F32Add),
            Operator::F32Sub => ops.push(WasmOp::F32Sub),
            Operator::F32Mul => ops.push(WasmOp::F32Mul),
            Operator::F32Div => ops.push(WasmOp::F32Div),
            Operator::F32Min => ops.push(WasmOp::F32Min),
            Operator::F32Max => ops.push(WasmOp::F32Max),
            Operator::F32Copysign => ops.push(WasmOp::F32Copysign),

            Operator::F64Abs => ops.push(WasmOp::F64Abs),
            Operator::F64Neg => ops.push(WasmOp::F64Neg),
            Operator::F64Ceil => ops.push(WasmOp::F64Ceil),
            Operator::F64Floor => ops.push(WasmOp::F64Floor),
            Operator::F64Trunc => ops.push(WasmOp::F64Trunc),
            Operator::F64Nearest => ops.push(WasmOp::F64Nearest),
            Operator::F64Sqrt => ops.push(WasmOp::F64Sqrt),
            Operator::F64Add => ops.push(WasmOp::F64Add),
            Operator::F64Sub => ops.push(WasmOp::F64Sub),
            Operator::F64Mul => ops.push(WasmOp::F64Mul),
            Operator::F64Div => ops.push(WasmOp::F64Div),
            Operator::F64Min => ops.push(WasmOp::F64Min),
            Operator::F64Max => ops.push(WasmOp::F64Max),
            Operator::F64Copysign => ops.push(WasmOp::F64Copysign),

            Operator::I32WrapI64 => ops.push(WasmOp::I32WrapI64),
            Operator::I32TruncF32S => ops.push(WasmOp::I32TruncF32S),
            Operator::I32TruncF32U => ops.push(WasmOp::I32TruncF32U),
            Operator::I32TruncF64S => ops.push(WasmOp::I32TruncF64S),
            Operator::I32TruncF64U => ops.push(WasmOp::I32TruncF64U),
            Operator::I64ExtendI32S => ops.push(WasmOp::I64ExtendI32S),
            Operator::I64ExtendI32U => ops.push(WasmOp::I64ExtendI32U),
            Operator::I64TruncF32S => ops.push(WasmOp::I64TruncF32S),
            Operator::I64TruncF32U => ops.push(WasmOp::I64TruncF32U),
            Operator::I64TruncF64S => ops.push(WasmOp::I64TruncF64S),
            Operator::I64TruncF64U => ops.push(WasmOp::I64TruncF64U),
            Operator::F32ConvertI32S => ops.push(WasmOp::F32ConvertI32S),
            Operator::F32ConvertI32U => ops.push(WasmOp::F32ConvertI32U),
            Operator::F32ConvertI64S => ops.push(WasmOp::F32ConvertI64S),
            Operator::F32ConvertI64U => ops.push(WasmOp::F32ConvertI64U),
            Operator::F32DemoteF64 => ops.push(WasmOp::F32DemoteF64),
            Operator::F64ConvertI32S => ops.push(WasmOp::F64ConvertI32S),
            Operator::F64ConvertI32U => ops.push(WasmOp::F64ConvertI32U),
            Operator::F64ConvertI64S => ops.push(WasmOp::F64ConvertI64S),
            Operator::F64ConvertI64U => ops.push(WasmOp::F64ConvertI64U),
            Operator::F64PromoteF32 => ops.push(WasmOp::F64PromoteF32),
            Operator::I32ReinterpretF32 => ops.push(WasmOp::I32ReinterpretF32),
            Operator::I64ReinterpretF64 => ops.push(WasmOp::I64ReinterpretF64),
            Operator::F32ReinterpretI32 => ops.push(WasmOp::F32ReinterpretI32),
            Operator::F64ReinterpretI64 => ops.push(WasmOp::F64ReinterpretI64),

            Operator::I32TruncSatF32S => ops.push(WasmOp::I32TruncSatF32S),
            Operator::I32TruncSatF32U => ops.push(WasmOp::I32TruncSatF32U),
            Operator::I32TruncSatF64S => ops.push(WasmOp::I32TruncSatF64S),
            Operator::I32TruncSatF64U => ops.push(WasmOp::I32TruncSatF64U),
            Operator::I64TruncSatF32S => ops.push(WasmOp::I64TruncSatF32S),
            Operator::I64TruncSatF32U => ops.push(WasmOp::I64TruncSatF32U),
            Operator::I64TruncSatF64S => ops.push(WasmOp::I64TruncSatF64S),
            Operator::I64TruncSatF64U => ops.push(WasmOp::I64TruncSatF64U),

            other => {
                if let Some(simd_op) = crate::simd::lower_simd_operator(&other) {
                    ops.push(WasmOp::Simd(Box::new(simd_op)));
                } else {
                    return Err(WasmError::UnsupportedFeature(format!(
                        "unsupported wasm instruction `{other:?}`"
                    )));
                }
            }
        }
    }

    Ok(CompiledWasmFunc {
        type_idx,
        extra_locals,
        ops: ops.into_boxed_slice(),
    })
}
