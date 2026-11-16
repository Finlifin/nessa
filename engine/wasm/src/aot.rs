//! Nessa `CompiledArtifact` -> WebAssembly (`.wasm`) AOT compiler.
//!
//! Lowers validated Nessa bytecode, type metadata, trait dispatch tables, and
//! constant pools directly into standalone W3C-valid WebAssembly 2.0 modules
//! targeting either `wasm32-wasi` (`wasi_snapshot_preview1`) or
//! `wasm32-freestanding`.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use nsbc::{
    AddrMode, CompiledArtifact, CompiledFunction, Constant, Instruction, InstructionData, Opcode,
    ParameterAbi, Reg,
};
use type_pool::{Intrinsic, TypeIndex, TypeKind};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ElementSection, Elements, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, GlobalSection, GlobalType, Ieee64,
    ImportSection, MemorySection, MemoryType, Module, RefType, TableSection, TableType,
    TypeSection, ValType,
};

use crate::aot_runtime::{
    FMT_SCRATCH_ADDR, GLOBAL_INITIALIZED, GLOBAL_RNG_STATE, GLOBAL_SLOT_SP, IMM_BOOL_TAG,
    IMM_CHAR_TAG, IMM_ENUM_TAG, IMM_I_SMALL_TAG, IMM_ISIZE_TAG, IMM_LOW7_MASK, IMM_NARROW_TAG,
    IMM_TRAIT_PROOF_TAG, IMM_TYPE_TAG, IMM_U_SMALL_TAG, IMM_USIZE_TAG, NARROW_F32, NARROW_I8,
    NARROW_I16, NARROW_I32, NARROW_U8, NARROW_U16, NARROW_U32, REG_PASS_BASE, ROLE_BOXED_SCALAR,
    ROLE_ERROR_ENVELOPE, ROLE_ORDINARY, ROLE_STRING_UTF8, ROT_F64_TAG_MASK, RtFuncIds,
    SLOT_STACK_BASE, SLOT_STACK_LIMIT, STATIC_DATA_BASE, StaticLayoutInfo, TAG_MASK, TK_ENUM,
    TK_ERROR, TK_FUNCTION, TK_INTRINSIC, TK_NEWTYPE, TK_OPTIONAL, TK_OTHER, TK_STRUCT, TK_TRAIT,
    TK_TUPLE, TY_CLOSURE, TY_I64, TY_STR, VAL_FALSE, VAL_NULL, VAL_TRUE, VAL_UNIT, WASI_CLOCK_ADDR,
    emit_from_f64_inline_const, emit_runtime_bodies, encode_f64_imm, encode_i64_imm,
    encode_u64_imm, mem, register_runtime_functions,
};

#[inline]
const fn f64c(v: f64) -> Ieee64 {
    Ieee64::new(v.to_bits())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmTarget {
    WasiPreview1,
    Freestanding,
}

impl WasmTarget {
    pub const fn triple(self) -> &'static str {
        match self {
            Self::WasiPreview1 => "wasm32-wasi",
            Self::Freestanding => "wasm32-freestanding",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "wasm" | "wasi" | "wasm32-wasi" | "wasm32-wasip1" => Some(Self::WasiPreview1),
            "freestanding" | "wasm32-freestanding" | "wasm32-unknown-unknown" => {
                Some(Self::Freestanding)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WasmCompileOptions {
    pub target: WasmTarget,
    pub export_all_pub: bool,
}

impl Default for WasmCompileOptions {
    fn default() -> Self {
        Self {
            target: WasmTarget::WasiPreview1,
            export_all_pub: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmCompileError {
    pub message: String,
}

impl WasmCompileError {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl fmt::Display for WasmCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WasmCompileError {}

// ── Static Data Builder ─────────────────────────────────────────────────────

struct StaticBlob {
    buf: Vec<u8>,
}

impl StaticBlob {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn current_addr(&self) -> u32 {
        STATIC_DATA_BASE + self.buf.len() as u32
    }

    fn align(&mut self, alignment: usize) -> u32 {
        while !(STATIC_DATA_BASE as usize + self.buf.len()).is_multiple_of(alignment) {
            self.buf.push(0);
        }
        self.current_addr()
    }

    fn push_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn alloc_static_string(&mut self, s: &str) -> u64 {
        self.align(8);
        // Header (16 bytes): [TY_STR, ROLE_STRING_UTF8, len, 0]
        self.push_u32(TY_STR);
        self.push_u32(ROLE_STRING_UTF8 as u32);
        self.push_u32(s.len() as u32);
        self.push_u32(0);
        let payload_addr = self.current_addr();
        self.buf.extend_from_slice(s.as_bytes());
        self.align(8);
        payload_addr as u64
    }

    fn alloc_boxed_u64(&mut self, ty: u32, bits: u64) -> u64 {
        self.align(8);
        self.push_u32(ty);
        self.push_u32(ROLE_BOXED_SCALAR as u32);
        self.push_u32(1);
        self.push_u32(0);
        let payload_addr = self.current_addr();
        self.push_u64(bits);
        payload_addr as u64
    }
}

// ── Scalar Export Signature Mapping ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarAbi {
    I64,
    U64,
    Isize,
    Usize,
    I32,
    U32,
    I16,
    U16,
    I8,
    U8,
    Bool,
    Char,
    F64,
    F32,
    Unit,
}

impl ScalarAbi {
    fn from_type(pool: &type_pool::TypePool, ty: TypeIndex) -> Option<Self> {
        let canon = pool.canonical_type(ty)?;
        match &pool.get(canon).kind {
            TypeKind::Intrinsic(intr) => match intr {
                Intrinsic::I64 => Some(Self::I64),
                Intrinsic::U64 => Some(Self::U64),
                Intrinsic::Isize => Some(Self::Isize),
                Intrinsic::Usize => Some(Self::Usize),
                Intrinsic::I32 => Some(Self::I32),
                Intrinsic::U32 => Some(Self::U32),
                Intrinsic::I16 => Some(Self::I16),
                Intrinsic::U16 => Some(Self::U16),
                Intrinsic::I8 => Some(Self::I8),
                Intrinsic::U8 => Some(Self::U8),
                Intrinsic::Bool => Some(Self::Bool),
                Intrinsic::Char => Some(Self::Char),
                Intrinsic::F64 => Some(Self::F64),
                Intrinsic::F32 => Some(Self::F32),
                Intrinsic::Unit => Some(Self::Unit),
                _ => None,
            },
            TypeKind::EffectQualified { inner, .. } => Self::from_type(pool, *inner),
            _ => None,
        }
    }

    fn wasm_val_type(self) -> Option<ValType> {
        match self {
            Self::I64 | Self::U64 | Self::Isize | Self::Usize => Some(ValType::I64),
            Self::I32
            | Self::U32
            | Self::I16
            | Self::U16
            | Self::I8
            | Self::U8
            | Self::Bool
            | Self::Char => Some(ValType::I32),
            Self::F64 => Some(ValType::F64),
            Self::F32 => Some(ValType::F32),
            Self::Unit => None,
        }
    }
}

struct ExportWrapperSpec {
    export_name: String,
    nessa_func_id: u32,
    params: Vec<ScalarAbi>,
    ret: ScalarAbi,
}

// ── Global-Aware Rapid Type Analysis (RTA) ──────────────────────────────────

fn is_supported_builtin_name(name: &str, _target: WasmTarget) -> bool {
    matches!(
        name,
        "print"
            | "println"
            | "eprint"
            | "eprintln"
            | "__io_flush_stdout"
            | "__io_flush_stderr"
            | "type_of"
            | "to_i64"
            | "to_f64"
            | "to_string"
            | "__scalar_eq"
            | "__scalar_cmp"
            | "__scalar_hash"
            | "__env_args"
            | "__iteration_step_done"
            | "__native_equal"
            | "__tuple_len"
            | "__tuple_get"
            | "__display_quote"
            | "__display_enter_tuple"
            | "__display_exit_tuple"
            | "abs"
            | "sin"
            | "cos"
            | "tan"
            | "sqrt"
            | "floor"
            | "ceil"
            | "round"
            | "trunc"
            | "fract"
            | "copysign"
            | "pow"
            | "powi"
            | "log"
            | "log2"
            | "log10"
            | "log_base"
            | "exp"
            | "exp2"
            | "expm1"
            | "log1p"
            | "cbrt"
            | "hypot"
            | "atan"
            | "atan2"
            | "asin"
            | "acos"
            | "sinh"
            | "cosh"
            | "tanh"
            | "min"
            | "max"
            | "clamp"
            | "sign"
            | "signum"
            | "recip"
            | "mul_add"
            | "div_euclid"
            | "rem_euclid"
            | "to_radians"
            | "to_degrees"
            | "round_ties_even"
            | "midpoint"
            | "is_nan"
            | "is_infinite"
            | "is_finite"
            | "is_normal"
            | "is_subnormal"
            | "is_sign_negative"
            | "is_sign_positive"
            | "to_bits"
            | "from_bits"
            | "pi"
            | "e"
            | "tau"
            | "ln_2"
            | "ln_10"
            | "log2_e"
            | "log10_e"
            | "sqrt_2"
            | "infinity"
            | "neg_infinity"
            | "nan"
            | "epsilon"
            | "min_positive"
            | "frac_pi_2"
            | "frac_pi_3"
            | "frac_pi_4"
            | "frac_pi_6"
            | "frac_pi_8"
            | "frac_1_pi"
            | "frac_2_pi"
            | "frac_2_sqrt_pi"
            | "frac_1_sqrt_2"
            | "log2_10"
            | "log10_2"
            | "count_ones"
            | "count_zeros"
            | "leading_zeros"
            | "trailing_zeros"
            | "leading_ones"
            | "trailing_ones"
            | "reverse_bits"
            | "swap_bytes"
            | "rotate_left"
            | "rotate_right"
            | "is_power_of_two"
            | "str_len"
            | "str_concat"
            | "str_is_empty"
            | "str_byte_at"
            | "str_contains"
            | "str_starts_with"
            | "str_ends_with"
            | "str_trim"
            | "str_trim_start"
            | "str_trim_end"
            | "str_trim_ascii"
            | "str_to_upper"
            | "str_to_lower"
            | "str_to_ascii_uppercase"
            | "str_to_ascii_lowercase"
            | "str_repeat"
            | "str_slice"
            | "str_byte_slice"
            | "str_char_len"
            | "str_char_at"
            | "str_is_ascii"
            | "char_to_code"
            | "char_from_code"
            | "char_into_string"
            | "char_is_whitespace"
            | "char_is_alphabetic"
            | "char_is_numeric"
            | "char_is_alphanumeric"
            | "char_is_uppercase"
            | "char_is_lowercase"
            | "char_is_ascii"
            | "char_is_ascii_digit"
            | "char_is_ascii_alphabetic"
            | "char_is_ascii_hexdigit"
            | "char_to_ascii_uppercase"
            | "char_to_ascii_lowercase"
            | "__list_init"
            | "__list_len"
            | "__list_get"
            | "__list_set"
            | "__list_push"
            | "__list_pop"
            | "__list_clear"
            | "__list_clone"
            | "__map_init"
            | "__map_len"
            | "__map_get"
            | "__map_set"
            | "__map_remove"
            | "__map_contains"
            | "__map_keys"
            | "__bytes_new"
            | "__bytes_with_capacity"
            | "__bytes_len"
            | "__bytes_get"
            | "__bytes_set"
            | "__bytes_push"
            | "__bytes_from_string"
            | "__bytes_to_string_lossy"
            | "__bytes_eq"
            | "exit"
            | "panic"
            | "__defer_push"
            | "__defer_pop"
            | "__time_monotonic_ns"
            | "__time_now_unix_ms"
            | "__rand_u64"
            | "__file_is_open"
    )
}

fn const_u32(constants: &[Constant], idx: u32) -> Option<u32> {
    match constants.get(idx as usize)? {
        Constant::Int(v) => u32::try_from(*v).ok(),
        Constant::UInt(v) => u32::try_from(*v).ok(),
        _ => None,
    }
}

fn compute_live_functions(
    artifact: &CompiledArtifact,
    exports: &[ExportWrapperSpec],
    target: WasmTarget,
) -> Result<HashSet<u32>, WasmCompileError> {
    let funcs = &artifact.codegen_output.functions;
    let constants = &artifact.codegen_output.constants;
    let globals = &artifact.codegen_output.globals;
    let pool = &artifact.type_pool;

    let builtin_names: HashMap<u32, &str> = artifact
        .builtins
        .iter()
        .map(|b| (b.id, b.name.as_str()))
        .collect();

    // Detect `NewClosure(thunk_fn, 0)` immediately followed by `StoreGlobal(g)` on an immutable
    // global inside `entry`, so unused stdlib `.builtin` thunks don't trigger false positives!
    let mut deferred_global_closures: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut deferred_closure_pcs: HashSet<(u32, usize)> = HashSet::new();

    if let Some(entry_id) = artifact.entry
        && let Some(entry_fn) = funcs.get(entry_id.0 as usize)
    {
        let words = &entry_fn.instructions;
        for pc in 0..words.len().saturating_sub(1) {
            let (Some(i0), Some(i1)) = (
                Instruction::decode(words[pc]),
                Instruction::decode(words[pc + 1]),
            ) else {
                continue;
            };
            let closure_target = match (i0.opcode, i0.data) {
                (Opcode::NewClosure, InstructionData::A { dst, base, imm12 }) if base.0 == 0 => {
                    Some((dst, imm12 as u32))
                }
                (Opcode::NewClosureWide, InstructionData::A { dst, base, imm12 })
                    if base.0 == 0 =>
                {
                    const_u32(constants, imm12 as u32).map(|fid| (dst, fid))
                }
                _ => None,
            };
            let Some((dst_reg, target_fid)) = closure_target else {
                continue;
            };
            let stored_global = match (i1.opcode, i1.data) {
                (Opcode::StoreGlobal, InstructionData::A { dst, base, imm12 })
                    if dst == dst_reg && base.0 == 0 =>
                {
                    Some(imm12 as u32)
                }
                (Opcode::StoreGlobalWide, InstructionData::A { dst, base, imm12 })
                    if dst == dst_reg =>
                {
                    Some(Instruction::wide_const_index(base, imm12))
                }
                _ => None,
            };
            if let Some(gidx) = stored_global
                && globals
                    .get(gidx as usize)
                    .is_some_and(|info| !info.is_mutable)
            {
                deferred_global_closures
                    .entry(gidx)
                    .or_default()
                    .push(target_fid);
                deferred_closure_pcs.insert((entry_id.0, pc));
            }
        }
    }

    let mut live_funcs: HashSet<u32> = HashSet::new();
    let mut worklist: Vec<u32> = Vec::new();
    let mut read_globals: HashSet<u32> = HashSet::new();
    let mut instantiated_types: HashSet<u32> = HashSet::new();
    let mut called_methods: HashSet<u32> = HashSet::new();
    let mut used_traits: HashSet<u32> = HashSet::new();

    // Intrinsics, tuples, and collection roles are always potentially instantiated
    for idx in 0..pool.len() as u32 {
        let ty = TypeIndex::from_raw(idx);
        if idx < Intrinsic::COUNT as u32
            || matches!(
                pool.get(ty).kind,
                TypeKind::Tuple { .. } | TypeKind::Function { .. }
            )
            || pool.checked_collection_role(ty).ok().flatten().is_some()
        {
            instantiated_types.insert(idx);
        }
    }

    if let Some(entry) = artifact.entry {
        live_funcs.insert(entry.0);
        worklist.push(entry.0);
    }
    for exp in exports {
        if live_funcs.insert(exp.nessa_func_id) {
            worklist.push(exp.nessa_func_id);
        }
    }

    loop {
        while let Some(fid) = worklist.pop() {
            let Some(func) = funcs.get(fid as usize) else {
                continue;
            };
            for (pc, &word) in func.instructions.iter().enumerate() {
                let Some(ins) = Instruction::decode(word) else {
                    continue;
                };
                match ins.opcode {
                    Opcode::Call => {
                        if let InstructionData::C { payload } = ins.data {
                            let (_, callee) = Instruction::c_arg_count_func_id(payload);
                            if live_funcs.insert(callee) {
                                worklist.push(callee);
                            }
                        }
                    }
                    Opcode::CallFar => {
                        if let InstructionData::C { payload } = ins.data {
                            let (_, cidx) = Instruction::c_arg_count_func_id(payload);
                            if let Some(callee) = const_u32(constants, cidx)
                                && live_funcs.insert(callee)
                            {
                                worklist.push(callee);
                            }
                        }
                    }
                    Opcode::NewClosure if !deferred_closure_pcs.contains(&(fid, pc)) => {
                        if let InstructionData::A { imm12, .. } = ins.data
                            && live_funcs.insert(imm12 as u32)
                        {
                            worklist.push(imm12 as u32);
                        }
                    }
                    Opcode::NewClosureWide if !deferred_closure_pcs.contains(&(fid, pc)) => {
                        if let InstructionData::A { imm12, .. } = ins.data
                            && let Some(callee) = const_u32(constants, imm12 as u32)
                            && live_funcs.insert(callee)
                        {
                            worklist.push(callee);
                        }
                    }
                    Opcode::LoadGlobal => {
                        if let InstructionData::A { imm12, .. } = ins.data {
                            let gidx = imm12 as u32;
                            if read_globals.insert(gidx)
                                && let Some(targets) = deferred_global_closures.get(&gidx)
                            {
                                for &t in targets {
                                    if live_funcs.insert(t) {
                                        worklist.push(t);
                                    }
                                }
                            }
                        }
                    }
                    Opcode::LoadGlobalWide => {
                        if let InstructionData::A { base, imm12, .. } = ins.data {
                            let gidx = Instruction::wide_const_index(base, imm12);
                            if read_globals.insert(gidx)
                                && let Some(targets) = deferred_global_closures.get(&gidx)
                            {
                                for &t in targets {
                                    if live_funcs.insert(t) {
                                        worklist.push(t);
                                    }
                                }
                            }
                        }
                    }
                    Opcode::NewObject => {
                        if let InstructionData::A { imm12, .. } = ins.data
                            && let Some(canon) =
                                pool.canonical_type(TypeIndex::from_raw(imm12 as u32))
                        {
                            instantiated_types.insert(canon.as_u32());
                        }
                    }
                    Opcode::NewEnum | Opcode::Load | Opcode::LoadConstWide => {
                        let cidx = match (ins.opcode, ins.amode, ins.data) {
                            (Opcode::NewEnum, _, InstructionData::A { imm12, .. }) => {
                                Some(imm12 as u32)
                            }
                            (Opcode::Load, AddrMode::Const, InstructionData::A { imm12, .. }) => {
                                Some(imm12 as u32)
                            }
                            (Opcode::LoadConstWide, _, InstructionData::A { base, imm12, .. }) => {
                                Some(Instruction::wide_const_index(base, imm12))
                            }
                            _ => None,
                        };
                        if let Some(ci) = cidx
                            && let Some(Constant::Enum { type_index, .. }) =
                                constants.get(ci as usize)
                            && let Some(canon) = pool.canonical_type(*type_index)
                        {
                            instantiated_types.insert(canon.as_u32());
                        }
                    }
                    Opcode::CallMethod => {
                        if let InstructionData::C { payload } = ins.data {
                            let (_, _, mid) = Instruction::c_call_method(payload);
                            called_methods.insert(mid);
                        }
                    }
                    Opcode::CallMethodFar => {
                        if let InstructionData::C { payload } = ins.data {
                            let (_, _, cidx) = Instruction::c_call_method(payload);
                            if let Some(mid) = const_u32(constants, cidx) {
                                called_methods.insert(mid);
                            }
                        }
                    }
                    Opcode::CallIndirect | Opcode::CallIndirectProof => {
                        called_methods.insert(str_interner::intern("apply").as_u32());
                    }
                    Opcode::TraitProof | Opcode::TraitProject => {
                        if let InstructionData::A { imm12, .. } = ins.data
                            && let Some(canon) =
                                pool.canonical_type(TypeIndex::from_raw(imm12 as u32))
                        {
                            // Include trait and its ancestor traits
                            let mut stack = vec![canon];
                            while let Some(cur) = stack.pop() {
                                if used_traits.insert(cur.as_u32())
                                    && let TypeKind::Trait { parents, .. } = &pool.get(cur).kind
                                {
                                    for &p in parents {
                                        if let Some(cp) = pool.canonical_type(p) {
                                            stack.push(cp);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Propagate RTA: instantiated types + called methods / used traits -> newly live methods
        let mut added = false;
        for &ty_raw in &instantiated_types {
            let ty = TypeIndex::from_raw(ty_raw);
            for slot in pool.methods_of(ty) {
                if called_methods.contains(&slot.name.as_u32()) && live_funcs.insert(slot.func_id) {
                    worklist.push(slot.func_id);
                    added = true;
                }
            }
        }
        for vt in pool.vtables_snapshot() {
            let Some(impl_canon) = pool.canonical_type(vt.implementor) else {
                continue;
            };
            let Some(trait_canon) = pool.canonical_type(vt.trait_type) else {
                continue;
            };
            if instantiated_types.contains(&impl_canon.as_u32())
                && used_traits.contains(&trait_canon.as_u32())
            {
                for &fid in &vt.entries {
                    if live_funcs.insert(fid) {
                        worklist.push(fid);
                        added = true;
                    }
                }
            }
        }
        if !added {
            break;
        }
    }

    // Check every live function for unsupported opcodes and unsupported host builtins
    let mut sorted_live: Vec<u32> = live_funcs.iter().copied().collect();
    sorted_live.sort_unstable();
    for fid in sorted_live {
        let Some(func) = funcs.get(fid as usize) else {
            continue;
        };
        let fn_name = str_interner::get(func.name);
        for &word in &func.instructions {
            let Some(ins) = Instruction::decode(word) else {
                return Err(WasmCompileError::new(format!(
                    "invalid bytecode instruction {word:#010x} in `{fn_name}`"
                )));
            };
            match ins.opcode {
                Opcode::EffectCallDyn
                | Opcode::PushHandlerClosure
                | Opcode::PushCapturingHandler
                | Opcode::ResetClosure
                | Opcode::ResumeContinuation
                | Opcode::ResumeContinuationOnce
                | Opcode::PopHandler
                | Opcode::AwaitPopHandler => {
                    return Err(WasmCompileError::new(format!(
                        "wasm target `{}` does not yet support algebraic effect / continuation opcode `{:?}` (in `{fn_name}`)",
                        target.triple(),
                        ins.opcode
                    )));
                }
                Opcode::AwaitFuture => {
                    return Err(WasmCompileError::new(format!(
                        "wasm target `{}` does not support multi-worker async `Future` awaiting (in `{fn_name}`)",
                        target.triple()
                    )));
                }
                Opcode::CallWasm => {
                    return Err(WasmCompileError::new(format!(
                        "wasm target `{}` cannot nest embedded `CallWasm` FFI bindings inside a standalone `.wasm` artifact (in `{fn_name}`)",
                        target.triple()
                    )));
                }
                Opcode::CallBuiltin => {
                    if let InstructionData::C { payload } = ins.data {
                        let (_, bid) = Instruction::c_arg_count_func_id(payload);
                        let bname = builtin_names.get(&bid).copied().unwrap_or("<unknown>");
                        if !is_supported_builtin_name(bname, target) {
                            return Err(WasmCompileError::new(format!(
                                "wasm target `{}` does not support host OS builtin `{bname}` (reachable from `{fn_name}`)",
                                target.triple()
                            )));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(live_funcs)
}

fn func_closure_uses_only_supported_wasm(
    funcs: &[CompiledFunction],
    constants: &[Constant],
    builtin_names: &HashMap<u32, &str>,
    root_fid: u32,
    target: WasmTarget,
) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![root_fid];
    seen.insert(root_fid);
    while let Some(fid) = stack.pop() {
        let Some(func) = funcs.get(fid as usize) else {
            return false;
        };
        for &word in &func.instructions {
            let Some(ins) = Instruction::decode(word) else {
                return false;
            };
            match ins.opcode {
                Opcode::EffectCallDyn
                | Opcode::PushHandlerClosure
                | Opcode::PushCapturingHandler
                | Opcode::ResetClosure
                | Opcode::ResumeContinuation
                | Opcode::ResumeContinuationOnce
                | Opcode::PopHandler
                | Opcode::AwaitPopHandler
                | Opcode::AwaitFuture
                | Opcode::CallWasm => return false,
                Opcode::CallBuiltin => {
                    if let InstructionData::C { payload } = ins.data {
                        let (_, bid) = Instruction::c_arg_count_func_id(payload);
                        let bname = builtin_names.get(&bid).copied().unwrap_or("<unknown>");
                        if !is_supported_builtin_name(bname, target) {
                            return false;
                        }
                    }
                }
                Opcode::Call => {
                    if let InstructionData::C { payload } = ins.data {
                        let (_, callee) = Instruction::c_arg_count_func_id(payload);
                        if seen.insert(callee) {
                            stack.push(callee);
                        }
                    }
                }
                Opcode::CallFar => {
                    if let InstructionData::C { payload } = ins.data {
                        let (_, cidx) = Instruction::c_arg_count_func_id(payload);
                        if let Some(callee) = const_u32(constants, cidx)
                            && seen.insert(callee)
                        {
                            stack.push(callee);
                        }
                    }
                }
                Opcode::NewClosure => {
                    if let InstructionData::A { imm12, .. } = ins.data
                        && seen.insert(imm12 as u32)
                    {
                        stack.push(imm12 as u32);
                    }
                }
                Opcode::NewClosureWide => {
                    if let InstructionData::A { imm12, .. } = ins.data
                        && let Some(callee) = const_u32(constants, imm12 as u32)
                        && seen.insert(callee)
                    {
                        stack.push(callee);
                    }
                }
                _ => {}
            }
        }
    }
    true
}

// ── Main AOT Compilation Entry Point ────────────────────────────────────────

pub fn compile_artifact_to_wasm(
    artifact: &CompiledArtifact,
    options: &WasmCompileOptions,
) -> Result<Vec<u8>, WasmCompileError> {
    let pool = &artifact.type_pool;
    let cg = &artifact.codegen_output;

    // 1. Discover exportable top-level user functions with scalar ABIs
    let mut export_specs = Vec::new();
    let mut seen_export_names = BTreeSet::new();
    if options.export_all_pub {
        let mut method_func_ids = HashSet::new();
        for idx in 0..pool.len() as u32 {
            for slot in pool.methods_of(TypeIndex::from_raw(idx)) {
                method_func_ids.insert(slot.func_id);
            }
        }
        for vt in pool.vtables_snapshot() {
            for &fid in &vt.entries {
                method_func_ids.insert(fid);
            }
        }

        let builtin_names: HashMap<u32, &str> = artifact
            .builtins
            .iter()
            .map(|b| (b.id, b.name.as_str()))
            .collect();

        for func in &cg.functions {
            if func.is_closure
                || func.display_owner.is_some()
                || method_func_ids.contains(&func.func_id.0)
                || Some(func.func_id) == artifact.entry
                || !func.abi.captures.is_empty()
                || func
                    .abi
                    .parameters
                    .iter()
                    .any(|p| *p != ParameterAbi::Value)
            {
                continue;
            }
            let raw_name = str_interner::get(func.name);
            if raw_name.is_empty()
                || raw_name.starts_with('_')
                || raw_name.contains('<')
                || raw_name.contains(':')
                || raw_name.contains('.')
                || matches!(
                    raw_name.as_str(),
                    "memory" | "_start" | "_initialize" | "nessa_main" | "nessa_alloc"
                )
            {
                continue;
            }
            if func.function_type == TypeIndex::INVALID {
                continue;
            }
            let Some(canon_fn_ty) = pool.canonical_type(func.function_type) else {
                continue;
            };
            let TypeKind::Function { params, ret } = &pool.get(canon_fn_ty).kind else {
                continue;
            };
            let Some(param_abis) = params
                .iter()
                .map(|&p| ScalarAbi::from_type(pool, p))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            if param_abis.contains(&ScalarAbi::Unit) {
                continue;
            }
            let Some(ret_abi) = ScalarAbi::from_type(pool, *ret) else {
                continue;
            };
            let probe_spec = ExportWrapperSpec {
                export_name: raw_name.clone(),
                nessa_func_id: func.func_id.0,
                params: param_abis,
                ret: ret_abi,
            };
            // Skip unused `std` OS wrappers (e.g. `sleep_ms`, `collect_garbage`) whose bodies call
            // host-only builtins; `artifact.entry` still enforces errors for any actually-used OS calls.
            if func_closure_uses_only_supported_wasm(
                &cg.functions,
                &cg.constants,
                &builtin_names,
                func.func_id.0,
                options.target,
            ) && seen_export_names.insert(raw_name)
            {
                export_specs.push(probe_spec);
            }
        }
    }

    // 2. Global-Aware RTA + Capability Validation
    let live_funcs = compute_live_functions(artifact, &export_specs, options.target)?;

    // 3. Build Static Memory Segment
    let mut blob = StaticBlob::new();

    // 3a. Module globals table (`16 * globals.len()` bytes)
    let globals_addr = blob.align(8);
    for g in &cg.globals {
        blob.push_u64(VAL_UNIT as u64);
        blob.push_u32(0); // init_flag = 0
        blob.push_u32(u32::from(g.is_mutable));
    }

    // 3b. Collect constants referenced by `live_funcs` so unreachable `std` constants
    // (such as unused `i128`/`u128` limits or unused error strings) neither bloat linear memory
    // nor trigger false-positive target diagnostics, while any reachable `i128`/`u128` constant
    // still fails cleanly at compile time.
    let mut referenced_consts = HashSet::new();
    for &fid in &live_funcs {
        if let Some(func) = cg.functions.get(fid as usize) {
            for &word in &func.instructions {
                let Some(ins) = Instruction::decode(word) else {
                    continue;
                };
                match (ins.opcode, ins.amode, ins.data) {
                    (Opcode::Load, AddrMode::Const, InstructionData::A { imm12, .. })
                    | (
                        Opcode::NewEnum
                        | Opcode::EnumIs
                        | Opcode::ErrorOk
                        | Opcode::ErrorErr
                        | Opcode::NewClosureWide,
                        _,
                        InstructionData::A { imm12, .. },
                    ) => {
                        referenced_consts.insert(imm12 as usize);
                    }
                    (Opcode::LoadConstWide, _, InstructionData::A { base, imm12, .. }) => {
                        referenced_consts
                            .insert(Instruction::wide_const_index(base, imm12) as usize);
                    }
                    (Opcode::CallFar, _, InstructionData::C { payload }) => {
                        let (_, cidx) = Instruction::c_arg_count_func_id(payload);
                        referenced_consts.insert(cidx as usize);
                    }
                    _ => {}
                }
            }
        }
    }

    let mut const_vals = Vec::with_capacity(cg.constants.len());
    for (cidx, c) in cg.constants.iter().enumerate() {
        if !referenced_consts.contains(&cidx) {
            const_vals.push(VAL_UNIT as u64);
            continue;
        }
        let encoded = match c {
            Constant::Int(v) => match encode_i64_imm(*v) {
                Some(imm) => imm,
                None => blob.alloc_boxed_u64(TY_I64, *v as u64),
            },
            Constant::UInt(v) => match encode_u64_imm(*v) {
                Some(imm) => imm,
                None => blob.alloc_boxed_u64(3, *v),
            },
            Constant::Int128(_) | Constant::UInt128(_) => {
                return Err(WasmCompileError::new(
                    "128-bit integer constants (`i128`/`u128`) are not yet supported in standalone wasm codegen",
                ));
            }
            Constant::Float(v) => encode_f64_imm(*v),
            Constant::Char(ch) => ((u64::from(*ch)) << 7) | (IMM_CHAR_TAG as u64),
            Constant::Type(ty) => {
                let canon = pool.canonical_type(*ty).unwrap_or(*ty);
                ((canon.as_u32() as u64) << 7) | (IMM_TYPE_TAG as u64)
            }
            Constant::Enum {
                type_index,
                variant,
            } => {
                let canon = pool.canonical_type(*type_index).unwrap_or(*type_index);
                let data = ((canon.as_u32() as u64) << 25) | (*variant as u64);
                (data << 7) | (IMM_ENUM_TAG as u64)
            }
            Constant::Str(s) => blob.alloc_static_string(s),
        };
        const_vals.push(encoded);
    }

    // 3c. Type Metadata Table (`32 * pool.len()` bytes)
    let type_meta_addr = blob.align(8);
    let type_count = pool.len() as u32;
    for idx in 0..type_count {
        let ty = TypeIndex::from_raw(idx);
        let canon = pool.canonical_type(ty).unwrap_or(ty);
        let info = pool.get(canon);
        let (kind_tag, count_or_inner, aux) = match &info.kind {
            TypeKind::Intrinsic(intr) => (TK_INTRINSIC, *intr as u32, 0),
            TypeKind::Struct { fields, .. } => (TK_STRUCT, fields.len() as u32, 0),
            TypeKind::Tuple { elements } => (TK_TUPLE, elements.len() as u32, 0),
            TypeKind::Enum { variants, .. } => {
                let max_fields = variants.iter().map(|v| v.fields.len()).max().unwrap_or(0);
                (TK_ENUM, (1 + max_fields) as u32, variants.len() as u32)
            }
            TypeKind::Newtype { inner, .. } => {
                let ci = pool.canonical_type(*inner).unwrap_or(*inner);
                (TK_NEWTYPE, ci.as_u32(), 0)
            }
            TypeKind::Optional { inner } => {
                let ci = pool.canonical_type(*inner).unwrap_or(*inner);
                (TK_OPTIONAL, ci.as_u32(), 0)
            }
            TypeKind::ErrorQualified { inner, .. } => {
                let ci = pool.canonical_type(*inner).unwrap_or(*inner);
                (TK_ERROR, ci.as_u32(), 0)
            }
            TypeKind::EffectQualified { inner, .. } => {
                let ci = pool.canonical_type(*inner).unwrap_or(*inner);
                if matches!(pool.get(ci).kind, TypeKind::ErrorQualified { .. }) {
                    let TypeKind::ErrorQualified { inner: ei, .. } = &pool.get(ci).kind else {
                        unreachable!()
                    };
                    (
                        TK_ERROR,
                        pool.canonical_type(*ei).unwrap_or(*ei).as_u32(),
                        0,
                    )
                } else {
                    (TK_OTHER, ci.as_u32(), 0)
                }
            }
            TypeKind::Trait { .. } => (TK_TRAIT, 0, 0),
            TypeKind::Function { params, .. } => (TK_FUNCTION, params.len() as u32, 0),
            _ => (TK_OTHER, 0, 0),
        };
        let stable_id = pool.stable_type_id(canon).unwrap_or(info.type_id);
        blob.push_u32(canon.as_u32());
        blob.push_u32(kind_tag);
        blob.push_u32(count_or_inner);
        blob.push_u32(aux);
        blob.push_u64(stable_id.hi());
        blob.push_u64(stable_id.lo());
    }

    // 3d. Method Dispatch Table (`16 * method_count` bytes)
    let mut method_rows: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut seen_methods = HashSet::new();
    for idx in 0..type_count {
        let ty = TypeIndex::from_raw(idx);
        let canon = pool.canonical_type(ty).unwrap_or(ty);
        for m in pool.methods_of(ty) {
            if !live_funcs.contains(&m.func_id) {
                continue;
            }
            let key = (canon.as_u32(), m.name.as_u32());
            if !seen_methods.insert(key) {
                continue;
            }
            let self_proof_view = cg
                .functions
                .get(m.func_id as usize)
                .and_then(|f| match f.abi.parameters.first() {
                    Some(ParameterAbi::Trait { view } | ParameterAbi::TraitSelf { view }) => {
                        Some(pool.canonical_type(*view).unwrap_or(*view).as_u32())
                    }
                    _ => None,
                })
                .unwrap_or(u32::MAX);
            method_rows.push((canon.as_u32(), m.name.as_u32(), m.func_id, self_proof_view));
        }
    }
    let method_table_addr = blob.align(8);
    for &(cty, mname, fid, spv) in &method_rows {
        blob.push_u32(cty);
        blob.push_u32(mname);
        blob.push_u32(fid);
        blob.push_u32(spv);
    }

    // 3e. Trait Implementation & VTable Tables
    let mut trait_rows: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut vtable_entries: Vec<(u32, u32)> = Vec::new();
    let mut seen_traits = HashSet::new();
    for vt in pool.vtables_snapshot() {
        let Some(impl_c) = pool.canonical_type(vt.implementor) else {
            continue;
        };
        let Some(trait_c) = pool.canonical_type(vt.trait_type) else {
            continue;
        };
        if !seen_traits.insert((impl_c.as_u32(), trait_c.as_u32())) {
            continue;
        }
        let start = vtable_entries.len() as u32;
        for &fid in &vt.entries {
            let self_proof_view = cg
                .functions
                .get(fid as usize)
                .and_then(|f| match f.abi.parameters.first() {
                    Some(ParameterAbi::Trait { view } | ParameterAbi::TraitSelf { view }) => {
                        Some(pool.canonical_type(*view).unwrap_or(*view).as_u32())
                    }
                    _ => None,
                })
                .unwrap_or(u32::MAX);
            vtable_entries.push((fid, self_proof_view));
        }
        trait_rows.push((
            impl_c.as_u32(),
            trait_c.as_u32(),
            start,
            vt.entries.len() as u32,
        ));
    }
    let trait_impl_addr = blob.align(8);
    for &(ic, tc, st, cnt) in &trait_rows {
        blob.push_u32(ic);
        blob.push_u32(tc);
        blob.push_u32(st);
        blob.push_u32(cnt);
    }
    let vtable_entries_addr = blob.align(8);
    for &(fid, spv) in &vtable_entries {
        blob.push_u32(fid);
        blob.push_u32(spv);
    }

    let heap_start = blob.align(16);
    let min_pages = (heap_start as u64).div_ceil(65536).max(3);

    // 4. Construct Wasm Sections
    let mut types = TypeSection::new();
    let mut imports = ImportSection::new();
    let mut functions = FunctionSection::new();
    let mut next_func_idx: u32 = 0;

    let (
        wasi_fd_write,
        wasi_proc_exit,
        wasi_clock_time_get,
        wasi_random_get,
        wasi_args_sizes_get,
        wasi_args_get,
    ) = match options.target {
        WasmTarget::WasiPreview1 => {
            let t_fd_write = types.len();
            types.ty().function(
                [ValType::I32, ValType::I32, ValType::I32, ValType::I32],
                [ValType::I32],
            );
            imports.import(
                "wasi_snapshot_preview1",
                "fd_write",
                EntityType::Function(t_fd_write),
            );
            let id_fd = next_func_idx;
            next_func_idx += 1;

            let t_exit = types.len();
            types.ty().function([ValType::I32], []);
            imports.import(
                "wasi_snapshot_preview1",
                "proc_exit",
                EntityType::Function(t_exit),
            );
            let id_exit = next_func_idx;
            next_func_idx += 1;

            let t_clock = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I64, ValType::I32], [ValType::I32]);
            imports.import(
                "wasi_snapshot_preview1",
                "clock_time_get",
                EntityType::Function(t_clock),
            );
            let id_clock = next_func_idx;
            next_func_idx += 1;

            let t_rand = types.len();
            types
                .ty()
                .function([ValType::I32, ValType::I32], [ValType::I32]);
            imports.import(
                "wasi_snapshot_preview1",
                "random_get",
                EntityType::Function(t_rand),
            );
            let id_rand = next_func_idx;
            next_func_idx += 1;

            imports.import(
                "wasi_snapshot_preview1",
                "args_sizes_get",
                EntityType::Function(t_rand),
            );
            let id_args_sizes = next_func_idx;
            next_func_idx += 1;

            imports.import(
                "wasi_snapshot_preview1",
                "args_get",
                EntityType::Function(t_rand),
            );
            let id_args_get = next_func_idx;
            next_func_idx += 1;

            (
                Some(id_fd),
                Some(id_exit),
                Some(id_clock),
                Some(id_rand),
                Some(id_args_sizes),
                Some(id_args_get),
            )
        }
        WasmTarget::Freestanding => (None, None, None, None, None, None),
    };

    let (rt, _rt_decls) = register_runtime_functions(
        options.target,
        &mut types,
        &mut functions,
        &mut next_func_idx,
    );

    let layout_info = StaticLayoutInfo {
        null_type_idx: pool.null_type().as_u32(),
        list_type_idx: pool.list_type().map_or(TY_I64, |t| t.as_u32()),
        map_type_idx: pool.map_type().map_or(TY_I64, |t| t.as_u32()),
        bytes_type_idx: pool.bytes_type().map_or(TY_I64, |t| t.as_u32()),
        type_meta_addr,
        type_count,
        method_table_addr,
        method_count: method_rows.len() as u32,
        trait_impl_addr,
        trait_impl_count: trait_rows.len() as u32,
        vtable_entries_addr,
        wasi_fd_write,
        wasi_proc_exit,
        wasi_clock_time_get,
        wasi_random_get,
        wasi_args_sizes_get,
        wasi_args_get,
    };

    // Map each Nessa FuncId(0..N) -> Wasm function index (live functions get lowered bodies;
    // dead functions alias `rt.trap_unreachable` in Table 0 without emitting dead code!).
    let mut nessa_to_wasm_fn: Vec<u32> = vec![rt.trap_unreachable; cg.functions.len()];
    let mut live_ordered: Vec<&CompiledFunction> = Vec::new();
    for func in &cg.functions {
        if live_funcs.contains(&func.func_id.0) {
            let widx = next_func_idx;
            next_func_idx += 1;
            functions.function(rt.nessa_fn_type);
            nessa_to_wasm_fn[func.func_id.0 as usize] = widx;
            live_ordered.push(func);
        }
    }

    // Auxiliary entry & export wrapper functions
    let ty_void_to_i64 = types.len();
    types.ty().function([], [ValType::I64]);
    let fn_init_run = next_func_idx;
    next_func_idx += 1;
    functions.function(ty_void_to_i64);

    let ty_void_to_void = types.len();
    types.ty().function([], []);
    let fn_initialize = next_func_idx;
    next_func_idx += 1;
    functions.function(ty_void_to_void);

    let fn_start = if options.target == WasmTarget::WasiPreview1 {
        let id = next_func_idx;
        next_func_idx += 1;
        functions.function(ty_void_to_void);
        Some(id)
    } else {
        None
    };

    let fn_nessa_main = next_func_idx;
    next_func_idx += 1;
    functions.function(ty_void_to_i64);

    let mut wrapper_wasm_ids = Vec::with_capacity(export_specs.len());
    for spec in &export_specs {
        let p_tys: Vec<ValType> = spec
            .params
            .iter()
            .filter_map(|p| p.wasm_val_type())
            .collect();
        let r_tys: Vec<ValType> = spec.ret.wasm_val_type().into_iter().collect();
        let wty = types.len();
        types.ty().function(p_tys, r_tys);
        let wid = next_func_idx;
        next_func_idx += 1;
        functions.function(wty);
        wrapper_wasm_ids.push(wid);
    }

    // Table 0: holds `nessa_to_wasm_fn[0..N]` so `call_indirect (func_id)` is O(1)!
    let mut tables = TableSection::new();
    tables.table(TableType {
        element_type: RefType::FUNCREF,
        minimum: nessa_to_wasm_fn.len().max(1) as u64,
        maximum: None,
        table64: false,
        shared: false,
    });

    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: min_pages,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut globals_sec = GlobalSection::new();
    // 0: GLOBAL_HEAP_PTR
    globals_sec.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(heap_start as i32),
    );
    // 1: GLOBAL_SLOT_SP
    globals_sec.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(SLOT_STACK_BASE),
    );
    // 2: GLOBAL_INITIALIZED
    globals_sec.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(0),
    );
    // 3: GLOBAL_RNG_STATE (deterministic SplitMix64 seed fallback)
    globals_sec.global(
        GlobalType {
            val_type: ValType::I64,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i64_const(0x9E37_79B9_7F4A_7C15_u64 as i64),
    );

    let mut exports_sec = ExportSection::new();
    exports_sec.export("memory", ExportKind::Memory, 0);
    if let Some(start_id) = fn_start {
        // WASI Command ABI forbids exporting both `_start` and `_initialize` simultaneously.
        exports_sec.export("_start", ExportKind::Func, start_id);
    } else {
        exports_sec.export("_initialize", ExportKind::Func, fn_initialize);
    }
    exports_sec.export("nessa_main", ExportKind::Func, fn_nessa_main);
    exports_sec.export("nessa_alloc", ExportKind::Func, rt.alloc);
    for (spec, &wid) in export_specs.iter().zip(&wrapper_wasm_ids) {
        exports_sec.export(&spec.export_name, ExportKind::Func, wid);
    }

    let mut elements_sec = ElementSection::new();
    if !nessa_to_wasm_fn.is_empty() {
        elements_sec.active(
            Some(0),
            &ConstExpr::i32_const(0),
            Elements::Functions(Cow::Borrowed(&nessa_to_wasm_fn)),
        );
    }

    // 5. Emit CodeSection
    let mut codes = CodeSection::new();
    emit_runtime_bodies(&mut codes, &rt, &layout_info);

    let lower_ctx = LowerCtx {
        artifact,
        rt: &rt,
        info: &layout_info,
        globals_addr,
        const_vals: &const_vals,
        nessa_to_wasm_fn: &nessa_to_wasm_fn,
        builtin_names: artifact
            .builtins
            .iter()
            .map(|b| (b.id, b.name.as_str()))
            .collect(),
    };

    for func in &live_ordered {
        let wasm_fn = lower_function(&lower_ctx, func)?;
        codes.function(&wasm_fn);
    }

    // Emit `fn_init_run`: runs `entry` once (returning its result or VAL_UNIT if already run)
    {
        let mut f = Function::new([]);
        let mut s = f.instructions();
        s.global_get(GLOBAL_INITIALIZED)
            .if_(BlockType::Empty)
            .i64_const(VAL_UNIT)
            .return_()
            .end()
            .i32_const(1)
            .global_set(GLOBAL_INITIALIZED);
        if let Some(entry) = artifact.entry
            && live_funcs.contains(&entry.0)
        {
            s.i32_const(0)
                .i32_const(0)
                .call(nessa_to_wasm_fn[entry.0 as usize]);
        } else {
            s.i64_const(VAL_UNIT);
        }
        s.end();
        codes.function(&f);
    }

    // Emit `fn_initialize`: `() -> ()`
    {
        let mut f = Function::new([]);
        f.instructions().call(fn_init_run).drop().end();
        codes.function(&f);
    }

    // Emit `fn_start`: `() -> ()` (checks uncaught `!Error` return from entry)
    if fn_start.is_some() {
        let mut f = Function::new([(1, ValType::I64), (1, ValType::I32)]);
        let mut s = f.instructions();
        s.call(fn_init_run)
            .local_tee(0)
            .i64_const(TAG_MASK)
            .i64_and()
            .i64_eqz()
            .local_get(0)
            .i64_const(0)
            .i64_ne()
            .i32_and()
            .if_(BlockType::Empty)
            .local_get(0)
            .i32_wrap_i64()
            .local_tee(1)
            .i32_const(12)
            .i32_sub()
            .i32_load(mem(0, 2))
            .i32_const(ROLE_ERROR_ENVELOPE)
            .i32_eq()
            .if_(BlockType::Empty)
            .local_get(1)
            .i64_load(mem(0, 3))
            .local_get(1)
            .i64_load(mem(8, 3))
            .i64_or()
            .i64_const(0)
            .i64_ne()
            .if_(BlockType::Empty);
        if let Some(proc_exit) = wasi_proc_exit {
            s.i32_const(1).call(proc_exit);
        }
        s.unreachable().end().end().end().end();
        codes.function(&f);
    }

    // Emit `fn_nessa_main`: `() -> i64`
    {
        let mut f = Function::new([(1, ValType::I64)]);
        let mut s = f.instructions();
        s.call(fn_init_run)
            .local_tee(0)
            .i64_const(VAL_UNIT)
            .i64_eq()
            .if_(BlockType::Result(ValType::I64))
            .i64_const(0)
            .else_()
            .local_get(0)
            .call(rt.to_i64)
            .end()
            .end();
        codes.function(&f);
    }

    // Emit scalar ABI export wrappers
    for spec in &export_specs {
        let wasm_fn = emit_export_wrapper(&lower_ctx, fn_init_run, spec);
        codes.function(&wasm_fn);
    }

    // 6. Data Section
    let mut data_sec = DataSection::new();
    if !blob.buf.is_empty() {
        data_sec.active(
            0,
            &ConstExpr::i32_const(STATIC_DATA_BASE as i32),
            blob.buf.iter().copied(),
        );
    }

    // 7. Assemble Module in canonical W3C section order
    let mut module = Module::new();
    module.section(&types);
    if !imports.is_empty() {
        module.section(&imports);
    }
    module.section(&functions);
    module.section(&tables);
    module.section(&memories);
    module.section(&globals_sec);
    module.section(&exports_sec);
    if !elements_sec.is_empty() {
        module.section(&elements_sec);
    }
    module.section(&codes);
    if !data_sec.is_empty() {
        module.section(&data_sec);
    }

    let bytes = module.finish();
    // Self-verify every emitted module with W3C `wasmparser::Validator`!
    crate::validate_wasm_bytes(&bytes).map_err(|e| {
        WasmCompileError::new(format!("internal wasm emission validation failed: {e}"))
    })?;
    Ok(bytes)
}

// ── Function Bytecode Lowering ──────────────────────────────────────────────

struct LowerCtx<'a> {
    artifact: &'a CompiledArtifact,
    rt: &'a RtFuncIds,
    info: &'a StaticLayoutInfo,
    globals_addr: u32,
    const_vals: &'a [u64],
    nessa_to_wasm_fn: &'a [u32],
    builtin_names: HashMap<u32, &'a str>,
}

// Local indices inside every lowered Nessa function:
//   0: $env_ptr (param i32)
//   1: $argc    (param i32)
//   2..34: $r0..$r31 (i64)
//   34: $bb (i32)
//   35: $frame_slots (i32)
//   36: $t0 (i32)
//   37: $t1 (i32)
//   38: $t2 (i32)
//   39: $t3 (i32)
//   40: $v0 (i64)
//   41: $v1 (i64)
//   42: $v2 (i64)
//   43: $f0 (f64)
//   44: $f1 (f64)
const L_ENV: u32 = 0;
const L_ARGC: u32 = 1;
const L_R0: u32 = 2;
const L_BB: u32 = 34;
const L_SLOTS: u32 = 35;
const L_T0: u32 = 36;
const L_T1: u32 = 37;
const L_T2: u32 = 38;
const L_T3: u32 = 39;
const L_V0: u32 = 40;
const L_V1: u32 = 41;
const L_V2: u32 = 42;
const L_F0: u32 = 43;
const L_F1: u32 = 44;

#[inline]
const fn r_loc(r: Reg) -> u32 {
    L_R0 + (r.0 as u32)
}

fn lower_function(
    ctx: &LowerCtx<'_>,
    func: &CompiledFunction,
) -> Result<Function, WasmCompileError> {
    let mut f = Function::new([
        (32, ValType::I64),
        (6, ValType::I32),
        (3, ValType::I64),
        (2, ValType::F64),
    ]);

    let words = &func.instructions;
    if words.is_empty() {
        f.instructions().i64_const(VAL_UNIT).end();
        return Ok(f);
    }

    // 1. Decode instructions and collect basic-block leaders
    let mut decoded = Vec::with_capacity(words.len());
    let mut leaders_set = BTreeSet::new();
    leaders_set.insert(0usize);

    for (pc, &w) in words.iter().enumerate() {
        let ins = Instruction::decode(w).ok_or_else(|| {
            WasmCompileError::new(format!(
                "invalid instruction {w:#010x} at PC {pc} in function {}",
                func.func_id.0
            ))
        })?;
        decoded.push(ins);
        match ins.opcode {
            Opcode::Jmp
            | Opcode::JmpFar
            | Opcode::JmpIf
            | Opcode::JmpIfNot
            | Opcode::JmpIfNull
            | Opcode::JmpIfNotNull => {
                if let InstructionData::J { offset, .. } = ins.data {
                    let target = ((pc as i64) + (offset as i64)) as usize;
                    if target <= words.len() {
                        leaders_set.insert(target);
                    }
                    if pc + 1 < words.len() {
                        leaders_set.insert(pc + 1);
                    }
                }
            }
            Opcode::Return | Opcode::ReturnUnit | Opcode::Unreachable | Opcode::MatchFail
                if pc + 1 < words.len() =>
            {
                leaders_set.insert(pc + 1);
            }
            _ => {}
        }
    }

    let leaders: Vec<usize> = leaders_set
        .into_iter()
        .filter(|&p| p < words.len())
        .collect();
    let num_blocks = leaders.len();
    let mut pc_to_block = BTreeMap::new();
    for (bidx, &lpc) in leaders.iter().enumerate() {
        pc_to_block.insert(lpc, bidx);
    }

    let mut s = f.instructions();

    // 2. Function Prologue: save `$frame_slots`, initialize registers to `VAL_UNIT`,
    //    unpack closure captures (if `$env_ptr != 0`) + explicit arguments from `REG_PASS_BASE`.
    s.global_get(GLOBAL_SLOT_SP).local_set(L_SLOTS);
    for reg_idx in 0..32u8 {
        s.i64_const(VAL_UNIT).local_set(r_loc(Reg(reg_idx)));
    }

    let cap_count = func.abi.capture_count();
    let explicit_phys = func
        .abi
        .physical_parameter_count()
        .saturating_sub(cap_count);
    if cap_count > 0 {
        // Unpack `0..cap_count` from `$env_ptr + 16 + i*8`
        s.local_get(L_ENV)
            .i32_eqz()
            .if_(BlockType::Empty)
            .unreachable()
            .end();
        for i in 0..cap_count {
            s.local_get(L_ENV)
                .i64_load(mem(16 + (i as u64) * 8, 3))
                .local_set(r_loc(Reg(i as u8)));
        }
        for i in 0..explicit_phys {
            s.i32_const(REG_PASS_BASE as i32)
                .i64_load(mem((i as u64) * 8, 3))
                .local_set(r_loc(Reg((cap_count + i) as u8)));
        }
    } else if explicit_phys > 0 {
        for i in 0..explicit_phys {
            s.i32_const(REG_PASS_BASE as i32)
                .i64_load(mem((i as u64) * 8, 3))
                .local_set(r_loc(Reg(i as u8)));
        }
    } else {
        // Variadic / dynamic ABI fallback (`function_type == INVALID` or runtime argc):
        // unpack up to `min($argc, 32)` if `$argc > 0`
        s.local_get(L_ARGC)
            .i32_const(0)
            .i32_gt_u()
            .if_(BlockType::Empty)
            .i32_const(REG_PASS_BASE as i32)
            .i64_load(mem(0, 3))
            .local_set(r_loc(Reg(0)))
            .end();
    }

    // 3. Structured CFG Dispatcher:
    //    (loop $dispatch
    //      (block $b_{M-1} ... (block $b_0 (local.get $bb) (br_table 0..M-1 0))
    //        <code of B_0>
    //      )
    //      ...
    //      <code of B_{M-1}>
    //    )
    s.loop_(BlockType::Empty);
    for _ in 0..num_blocks {
        s.block(BlockType::Empty);
    }
    let table_targets: Vec<u32> = (0..num_blocks as u32).collect();
    s.local_get(L_BB).br_table(table_targets, 0);

    for (bidx, &start_pc) in leaders.iter().enumerate() {
        let end_pc = leaders.get(bidx + 1).copied().unwrap_or(words.len());
        // Close `$b_{bidx}` so breaks targeting `bidx` land right here!
        s.end();

        // Depth of `$dispatch` loop from inside `<code of B_{bidx}>` is `num_blocks - 1 - bidx`.
        let dispatch_depth = (num_blocks - 1 - bidx) as u32;

        for (pc, ins) in decoded[start_pc..end_pc].iter().enumerate() {
            let abs_pc = start_pc + pc;
            lower_instruction(ctx, &mut s, ins, abs_pc, bidx, dispatch_depth, &pc_to_block)?;
        }
    }

    // End of last block if it fell through: restore `$slot_sp` and return `VAL_UNIT`
    s.local_get(L_SLOTS)
        .global_set(GLOBAL_SLOT_SP)
        .i64_const(VAL_UNIT)
        .return_()
        .end(); // end loop $dispatch

    s.i64_const(VAL_UNIT).end();
    Ok(f)
}

fn emit_jump_to_block(
    s: &mut wasm_encoder::InstructionSink<'_>,
    cur_block: usize,
    target_block: usize,
    dispatch_depth: u32,
) {
    if target_block == cur_block + 1 {
        // Fallthrough to immediately following block? Only valid if at very end of block,
        // so use direct forward break `br 0` which works from ANY point in `cur_block`!
        s.br(0);
    } else if target_block > cur_block {
        let rel = (target_block - cur_block - 1) as u32;
        s.br(rel);
    } else {
        s.i32_const(target_block as i32)
            .local_set(L_BB)
            .br(dispatch_depth);
    }
}

fn emit_cond_jump_to_block(
    s: &mut wasm_encoder::InstructionSink<'_>,
    cur_block: usize,
    target_block: usize,
    dispatch_depth: u32,
) {
    // Assumes `i32` condition (`0` or non-zero) is on top of the Wasm value stack!
    if target_block > cur_block {
        let rel = (target_block - cur_block - 1) as u32;
        s.br_if(rel);
    } else {
        s.if_(BlockType::Empty)
            .i32_const(target_block as i32)
            .local_set(L_BB)
            .br(dispatch_depth + 1)
            .end();
    }
}

fn emit_is_truthy(s: &mut wasm_encoder::InstructionSink<'_>, reg: Reg, rt: &RtFuncIds) {
    // Matches `interpreter::is_truthy`:
    //   null / unit / false -> 0
    //   true -> 1
    //   0 / 0.0 -> 0
    //   everything else -> 1
    s.local_get(r_loc(reg))
        .i64_const(VAL_FALSE)
        .i64_eq()
        .local_get(r_loc(reg))
        .i64_const(VAL_NULL)
        .i64_eq()
        .i32_or()
        .local_get(r_loc(reg))
        .i64_const(VAL_UNIT)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Result(ValType::I32))
        .i32_const(0)
        .else_()
        .local_get(r_loc(reg))
        .i64_const(VAL_TRUE)
        .i64_eq()
        .if_(BlockType::Result(ValType::I32))
        .i32_const(1)
        .else_()
        // IMM_I_SMALL == 0? (`encode_i64_imm(0) == 0x01`)
        .local_get(r_loc(reg))
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .if_(BlockType::Result(ValType::I32))
        .i32_const(0)
        .else_()
        // Float zero?
        .local_get(r_loc(reg))
        .i64_const(ROT_F64_TAG_MASK)
        .i64_and()
        .i64_const(0)
        .i64_ne()
        .local_get(r_loc(reg))
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(0x11)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Result(ValType::I32))
        .local_get(r_loc(reg))
        .call(rt.to_f64)
        .f64_const(f64c(0.0))
        .f64_ne()
        .else_()
        .i32_const(1)
        .end()
        .end()
        .end()
        .end();
}

fn lower_instruction(
    ctx: &LowerCtx<'_>,
    s: &mut wasm_encoder::InstructionSink<'_>,
    ins: &Instruction,
    pc: usize,
    cur_block: usize,
    dispatch_depth: u32,
    pc_to_block: &BTreeMap<usize, usize>,
) -> Result<(), WasmCompileError> {
    let rt = ctx.rt;
    match ins.opcode {
        // ── Arithmetic & Bitwise ────────────────────────────────────────
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod => {
            let InstructionData::R { dst, src1, src2 } = ins.data else {
                unreachable!()
            };
            let helper = match ins.opcode {
                Opcode::Add => rt.add,
                Opcode::Sub => rt.sub,
                Opcode::Mul => rt.mul,
                Opcode::Div => rt.div,
                _ => rt.rem,
            };
            s.local_get(r_loc(src1))
                .local_get(r_loc(src2))
                .call(helper)
                .local_set(r_loc(dst));
        }
        Opcode::Neg => {
            let InstructionData::R { dst, src1, .. } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(src1)).call(rt.neg).local_set(r_loc(dst));
        }
        Opcode::BitAnd
        | Opcode::BitOr
        | Opcode::BitXor
        | Opcode::Shl
        | Opcode::Shr
        | Opcode::UShr => {
            let InstructionData::R { dst, src1, src2 } = ins.data else {
                unreachable!()
            };
            // Preserve narrow/usize/u64 tag of `src1`
            s.local_get(r_loc(src1))
                .local_set(L_V2)
                .local_get(r_loc(src1))
                .call(rt.to_i64)
                .local_get(r_loc(src2))
                .call(rt.to_i64);
            match ins.opcode {
                Opcode::BitAnd => {
                    s.i64_and();
                }
                Opcode::BitOr => {
                    s.i64_or();
                }
                Opcode::BitXor => {
                    s.i64_xor();
                }
                Opcode::Shl => {
                    s.i64_shl();
                }
                Opcode::Shr => {
                    s.i64_shr_s();
                }
                _ => {
                    s.i64_shr_u();
                }
            }
            s.local_set(L_V0);
            retag_like_lhs(s, rt, L_V2, L_V0, r_loc(dst));
        }
        Opcode::BitNot => {
            let InstructionData::R { dst, src1, .. } = ins.data else {
                unreachable!()
            };
            // Check bool logical not vs integer bitwise not!
            s.local_get(r_loc(src1))
                .i64_const(IMM_LOW7_MASK)
                .i64_and()
                .i64_const(IMM_BOOL_TAG)
                .i64_eq()
                .if_(BlockType::Empty)
                .local_get(r_loc(src1))
                .i64_const(0x80)
                .i64_xor()
                .local_set(r_loc(dst))
                .else_()
                .local_get(r_loc(src1))
                .local_set(L_V2)
                .local_get(r_loc(src1))
                .call(rt.to_i64)
                .i64_const(-1)
                .i64_xor()
                .local_set(L_V0);
            retag_like_lhs(s, rt, L_V2, L_V0, r_loc(dst));
            s.end();
        }

        // ── Comparisons ─────────────────────────────────────────────────
        Opcode::CmpEq
        | Opcode::CmpNe
        | Opcode::CmpLt
        | Opcode::CmpLe
        | Opcode::CmpGt
        | Opcode::CmpGe => {
            let InstructionData::R { dst, src1, src2 } = ins.data else {
                unreachable!()
            };
            let op_code = match ins.opcode {
                Opcode::CmpEq => 0,
                Opcode::CmpNe => 1,
                Opcode::CmpLt => 2,
                Opcode::CmpLe => 3,
                Opcode::CmpGt => 4,
                _ => 5,
            };
            s.local_get(r_loc(src1))
                .local_get(r_loc(src2))
                .i32_const(op_code)
                .call(rt.cmp)
                .local_set(r_loc(dst));
        }

        // ── Register Moves ──────────────────────────────────────────────
        Opcode::Mov => {
            let InstructionData::R { dst, src1, .. } = ins.data else {
                unreachable!()
            };
            if dst != src1 {
                s.local_get(r_loc(src1)).local_set(r_loc(dst));
            }
        }
        Opcode::Swap => {
            let InstructionData::R { dst, src1, .. } = ins.data else {
                unreachable!()
            };
            if dst != src1 {
                s.local_get(r_loc(dst))
                    .local_set(L_V0)
                    .local_get(r_loc(src1))
                    .local_set(r_loc(dst))
                    .local_get(L_V0)
                    .local_set(r_loc(src1));
            }
        }

        // ── Loads & Slots ───────────────────────────────────────────────
        Opcode::AllocateSlots => {
            let InstructionData::E { payload } = ins.data else {
                unreachable!()
            };
            let bytes = (payload as i32) * 8;
            if bytes > 0 {
                s.local_get(L_SLOTS)
                    .i32_const(bytes)
                    .i32_add()
                    .local_tee(L_T0)
                    .i32_const(SLOT_STACK_LIMIT)
                    .i32_gt_u()
                    .if_(BlockType::Empty)
                    .unreachable()
                    .end()
                    .local_get(L_T0)
                    .global_set(GLOBAL_SLOT_SP);
            }
        }
        Opcode::LoadSlot => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let idx = ((base.0 as u64) << 12) | (imm12 as u64);
            s.local_get(L_SLOTS)
                .i64_load(mem(idx * 8, 3))
                .local_set(r_loc(dst));
        }
        Opcode::StoreSlot => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let idx = ((base.0 as u64) << 12) | (imm12 as u64);
            s.local_get(L_SLOTS)
                .local_get(r_loc(dst))
                .i64_store(mem(idx * 8, 3));
        }
        Opcode::Load => {
            let InstructionData::A { dst, imm12, .. } = ins.data else {
                unreachable!()
            };
            match ins.amode {
                AddrMode::Imm => {
                    let v = Instruction::sext_imm12(imm12) as i64;
                    let raw = encode_i64_imm(v).unwrap() as i64;
                    s.i64_const(raw).local_set(r_loc(dst));
                }
                AddrMode::Const => {
                    let raw = ctx
                        .const_vals
                        .get(imm12 as usize)
                        .copied()
                        .unwrap_or(VAL_UNIT as u64) as i64;
                    s.i64_const(raw).local_set(r_loc(dst));
                }
            }
        }
        Opcode::LoadConstWide => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let idx = Instruction::wide_const_index(base, imm12) as usize;
            let raw = ctx.const_vals.get(idx).copied().unwrap_or(VAL_UNIT as u64) as i64;
            s.i64_const(raw).local_set(r_loc(dst));
        }
        Opcode::LoadUnit => {
            let InstructionData::A { dst, .. } = ins.data else {
                unreachable!()
            };
            s.i64_const(VAL_UNIT).local_set(r_loc(dst));
        }
        Opcode::LoadTrue => {
            let InstructionData::A { dst, .. } = ins.data else {
                unreachable!()
            };
            s.i64_const(VAL_TRUE).local_set(r_loc(dst));
        }
        Opcode::LoadFalse => {
            let InstructionData::A { dst, .. } = ins.data else {
                unreachable!()
            };
            s.i64_const(VAL_FALSE).local_set(r_loc(dst));
        }
        Opcode::LoadNull => {
            let InstructionData::A { dst, .. } = ins.data else {
                unreachable!()
            };
            s.i64_const(VAL_NULL).local_set(r_loc(dst));
        }

        // ── Globals ─────────────────────────────────────────────────────
        Opcode::LoadGlobal | Opcode::LoadGlobalWide => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let gidx = if ins.opcode == Opcode::LoadGlobalWide {
                Instruction::wide_const_index(base, imm12)
            } else {
                imm12 as u32
            };
            let slot_addr = ctx.globals_addr as u64 + (gidx as u64) * 16;
            // Trap if uninitialized (`init_flag == 0`)
            s.i32_const(0)
                .i32_load(mem(slot_addr + 8, 2))
                .i32_eqz()
                .if_(BlockType::Empty)
                .unreachable()
                .end()
                .i32_const(0)
                .i64_load(mem(slot_addr, 3))
                .local_set(r_loc(dst));
        }
        Opcode::StoreGlobal | Opcode::StoreGlobalWide => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let gidx = if ins.opcode == Opcode::StoreGlobalWide {
                Instruction::wide_const_index(base, imm12)
            } else {
                imm12 as u32
            };
            let slot_addr = ctx.globals_addr as u64 + (gidx as u64) * 16;
            // Enforce const-once if `is_mut == 0 && init_flag != 0`
            s.i32_const(0)
                .i32_load(mem(slot_addr + 12, 2))
                .i32_eqz()
                .i32_const(0)
                .i32_load(mem(slot_addr + 8, 2))
                .i32_const(0)
                .i32_ne()
                .i32_and()
                .if_(BlockType::Empty)
                .unreachable()
                .end()
                .i32_const(0)
                .local_get(r_loc(dst))
                .i64_store(mem(slot_addr, 3))
                .i32_const(0)
                .i32_const(1)
                .i32_store(mem(slot_addr + 8, 2));
        }

        // ── Objects, Structs, Tuples ────────────────────────────────────
        Opcode::NewObject => {
            let InstructionData::A { dst, imm12, .. } = ins.data else {
                unreachable!()
            };
            let ty = TypeIndex::from_raw(imm12 as u32);
            let canon = ctx.artifact.type_pool.canonical_type(ty).unwrap_or(ty);
            let words = match &ctx.artifact.type_pool.get(canon).kind {
                TypeKind::Struct { fields, .. } => fields.len(),
                TypeKind::Tuple { elements } => elements.len(),
                TypeKind::Newtype { .. } => 1,
                _ => 0,
            };
            s.i32_const(canon.as_u32() as i32)
                .i32_const(ROLE_ORDINARY)
                .i32_const(words as i32)
                .i32_const(0)
                .call(rt.alloc_obj)
                .local_set(L_T0);
            for i in 0..words {
                s.local_get(L_T0)
                    .i64_const(VAL_UNIT)
                    .i64_store(mem((i as u64) * 8, 3));
            }
            s.local_get(L_T0).i64_extend_i32_u().local_set(r_loc(dst));
        }
        Opcode::LoadField => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i32_wrap_i64()
                .i64_load(mem((imm12 as u64) * 8, 3))
                .local_set(r_loc(dst));
        }
        Opcode::StoreField => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i32_wrap_i64()
                .local_get(r_loc(dst))
                .i64_store(mem((imm12 as u64) * 8, 3));
        }

        // ── Enums & Pattern Matching ────────────────────────────────────
        Opcode::NewEnum => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let desc_raw = ctx
                .const_vals
                .get(imm12 as usize)
                .copied()
                .unwrap_or(VAL_UNIT as u64);
            let enum_ty = TypeIndex::from_raw(((desc_raw >> 32) & 0x7fff_ffff) as u32);
            let variant = ((desc_raw >> 7) & ((1 << 25) - 1)) as u32;
            let canon = ctx
                .artifact
                .type_pool
                .canonical_type(enum_ty)
                .unwrap_or(enum_ty);
            let (total_words, field_count) = match &ctx.artifact.type_pool.get(canon).kind {
                TypeKind::Enum { variants, .. } => {
                    let max_f = variants.iter().map(|v| v.fields.len()).max().unwrap_or(0);
                    let cur_f = variants
                        .iter()
                        .find(|v| v.tag == variant)
                        .map_or(0, |v| v.fields.len());
                    (1 + max_f, cur_f)
                }
                _ => (1, 0),
            };
            if field_count == 0 {
                s.i64_const(desc_raw as i64).local_set(r_loc(dst));
            } else {
                // `base` holds tuple of `field_count` arguments
                s.local_get(r_loc(base))
                    .i32_wrap_i64()
                    .local_set(L_T1)
                    .i32_const(canon.as_u32() as i32)
                    .i32_const(ROLE_ORDINARY)
                    .i32_const(total_words as i32)
                    .i32_const(0)
                    .call(rt.alloc_obj)
                    .local_tee(L_T0)
                    .i64_const(desc_raw as i64)
                    .i64_store(mem(0, 3));
                for i in 0..(total_words - 1) {
                    if i < field_count {
                        s.local_get(L_T0)
                            .local_get(L_T1)
                            .i64_load(mem((i as u64) * 8, 3))
                            .i64_store(mem(8 + (i as u64) * 8, 3));
                    } else {
                        s.local_get(L_T0)
                            .i64_const(VAL_UNIT)
                            .i64_store(mem(8 + (i as u64) * 8, 3));
                    }
                }
                s.local_get(L_T0).i64_extend_i32_u().local_set(r_loc(dst));
            }
        }
        Opcode::EnumIs => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let desc_raw = ctx
                .const_vals
                .get(imm12 as usize)
                .copied()
                .unwrap_or(VAL_UNIT as u64) as i64;
            // Matches either inline `IMM_ENUM == desc_raw` OR heap enum whose `word[0] == desc_raw`!
            s.local_get(r_loc(base))
                .i64_const(desc_raw)
                .i64_eq()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .local_get(r_loc(base))
                .i64_const(TAG_MASK)
                .i64_and()
                .i64_eqz()
                .local_get(r_loc(base))
                .i64_const(0)
                .i64_ne()
                .i32_and()
                .if_(BlockType::Result(ValType::I64))
                .local_get(r_loc(base))
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(12)
                .i32_sub()
                .i32_load(mem(0, 2))
                .i32_const(ROLE_ORDINARY)
                .i32_eq()
                .local_get(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .i32_const(0)
                .i32_gt_u()
                .i32_and()
                .if_(BlockType::Result(ValType::I64))
                .local_get(L_T0)
                .i64_load(mem(0, 3))
                .i64_const(desc_raw)
                .i64_eq()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .end()
                .local_set(r_loc(dst));
        }
        Opcode::EnumField => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i32_wrap_i64()
                .i64_load(mem(8 + (imm12 as u64) * 8, 3))
                .local_set(r_loc(dst));
        }
        Opcode::MatchFail | Opcode::Unreachable => {
            s.unreachable();
        }

        // ── Error Envelopes (`!E T`) ────────────────────────────────────
        Opcode::ErrorOk | Opcode::ErrorErr => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let ty_val = ctx.const_vals.get(imm12 as usize).copied().unwrap_or(0);
            let target_ty = ((ty_val >> 7) as u32).min(ctx.info.type_count.saturating_sub(1));
            s.local_get(r_loc(base)).local_set(L_V0);
            if ins.opcode == Opcode::ErrorOk {
                s.i32_const(target_ty as i32)
                    .i32_const(ROLE_ERROR_ENVELOPE)
                    .i32_const(3)
                    .i32_const(0)
                    .call(rt.alloc_obj)
                    .local_tee(L_T0)
                    .i64_const(0)
                    .i64_store(mem(0, 3))
                    .local_get(L_T0)
                    .i64_const(0)
                    .i64_store(mem(8, 3))
                    .local_get(L_T0)
                    .local_get(L_V0)
                    .i64_store(mem(16, 3))
                    .local_get(L_T0)
                    .i64_extend_i32_u()
                    .local_set(r_loc(dst));
            } else {
                // Lookup concrete type's `TypeId(hi, lo)` from `type_meta_addr + concrete * 32 + 16`
                s.local_get(L_V0)
                    .call(rt.reflected_type)
                    .i32_const(5)
                    .i32_shl()
                    .i32_const(ctx.info.type_meta_addr as i32)
                    .i32_add()
                    .local_set(L_T1)
                    .i32_const(target_ty as i32)
                    .i32_const(ROLE_ERROR_ENVELOPE)
                    .i32_const(3)
                    .i32_const(0)
                    .call(rt.alloc_obj)
                    .local_tee(L_T0)
                    .local_get(L_T1)
                    .i64_load(mem(16, 3))
                    .local_tee(L_V1)
                    .i64_store(mem(0, 3))
                    .local_get(L_T0)
                    .local_get(L_T1)
                    .i64_load(mem(24, 3))
                    .local_tee(L_V2)
                    .i64_store(mem(8, 3))
                    // Ensure non-zero error tag even if structural type had TypeId::ZERO
                    .local_get(L_V1)
                    .local_get(L_V2)
                    .i64_or()
                    .i64_eqz()
                    .if_(BlockType::Empty)
                    .local_get(L_T0)
                    .i64_const(1)
                    .i64_store(mem(8, 3))
                    .end()
                    .local_get(L_T0)
                    .local_get(L_V0)
                    .i64_store(mem(16, 3))
                    .local_get(L_T0)
                    .i64_extend_i32_u()
                    .local_set(r_loc(dst));
            }
        }
        Opcode::ErrorIsOk => {
            let InstructionData::A { dst, base, .. } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i64_load(mem(0, 3))
                .local_get(L_T0)
                .i64_load(mem(8, 3))
                .i64_or()
                .i64_eqz()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r_loc(dst));
        }
        Opcode::ErrorPayload => {
            let InstructionData::A { dst, base, .. } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i32_wrap_i64()
                .i64_load(mem(16, 3))
                .local_set(r_loc(dst));
        }

        // ── Collections & Closures ──────────────────────────────────────
        Opcode::NewList => {
            let InstructionData::A { dst, imm12, .. } = ins.data else {
                unreachable!()
            };
            s.i32_const(imm12 as i32)
                .call(rt.list_new)
                .local_set(r_loc(dst));
        }
        Opcode::NewMap => {
            let InstructionData::A { dst, imm12, .. } = ins.data else {
                unreachable!()
            };
            s.i32_const(imm12 as i32)
                .call(rt.map_new)
                .local_set(r_loc(dst));
        }
        Opcode::LoadIndex => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .local_get(r_loc(Reg(imm12 as u8)))
                .call(rt.list_get)
                .local_set(r_loc(dst));
        }
        Opcode::StoreIndex => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .local_get(r_loc(Reg(imm12 as u8)))
                .local_get(r_loc(dst))
                .call(rt.list_set);
        }
        Opcode::NewClosure | Opcode::NewClosureWide => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let target_fid = if ins.opcode == Opcode::NewClosure {
                imm12 as u32
            } else {
                const_u32(&ctx.artifact.codegen_output.constants, imm12 as u32).unwrap_or(0)
            };
            let cap_count = base.0 as usize;
            s.i32_const(TY_CLOSURE as i32)
                .i32_const(ROLE_ORDINARY)
                .i32_const((2 + cap_count) as i32)
                .i32_const(0)
                .call(rt.alloc_obj)
                .local_tee(L_T0)
                .i64_const(target_fid as i64)
                .i64_store(mem(0, 3))
                .local_get(L_T0)
                .i64_const(cap_count as i64)
                .i64_store(mem(8, 3));
            for i in 0..cap_count {
                s.local_get(L_T0)
                    .local_get(r_loc(Reg(i as u8)))
                    .i64_store(mem(16 + (i as u64) * 8, 3));
            }
            s.local_get(L_T0).i64_extend_i32_u().local_set(r_loc(dst));
        }

        // ── Type Predicates & Casts ─────────────────────────────────────
        Opcode::TypeCheck => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i32_const(imm12 as i32)
                .call(rt.type_check)
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r_loc(dst));
        }
        Opcode::TypeAssert | Opcode::TypeCast | Opcode::TypeCastSafe => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            let mode = match ins.opcode {
                Opcode::TypeAssert => 0,
                Opcode::TypeCast => 1,
                _ => 2,
            };
            s.local_get(r_loc(base))
                .i32_const(imm12 as i32)
                .i32_const(mode)
                .call(rt.type_cast)
                .local_set(r_loc(dst));
        }

        // ── Trait Proofs & Dynamic Dispatch ─────────────────────────────
        Opcode::TraitProof | Opcode::TraitProject => {
            let InstructionData::A { dst, base, imm12 } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i32_const(imm12 as i32)
                .call(rt.trait_proof)
                .local_set(r_loc(dst));
        }
        Opcode::TraitAssert => {
            // `base` = proof, `dst` = data -> validate proof tag, keep `dst` unchanged
            let InstructionData::A { base, .. } = ins.data else {
                unreachable!()
            };
            s.local_get(r_loc(base))
                .i64_const(IMM_LOW7_MASK)
                .i64_and()
                .i64_const(IMM_TRAIT_PROOF_TAG)
                .i64_ne()
                .if_(BlockType::Empty)
                .unreachable()
                .end();
        }
        Opcode::TraitCall => {
            let InstructionData::C { payload } = ins.data else {
                unreachable!()
            };
            let (arg_count, proof_reg, slot) = Instruction::c_call_method(payload);
            let argc = arg_count as usize;
            s.local_get(r_loc(proof_reg))
                .local_set(L_V0)
                .local_get(L_V0)
                .i32_const(slot as i32)
                .call(rt.trait_vtable_func)
                .local_set(L_V1)
                // L_T0 = func_id, L_T1 = self_proof_view
                .local_get(L_V1)
                .i32_wrap_i64()
                .local_set(L_T0)
                .local_get(L_V1)
                .i64_const(32)
                .i64_shr_u()
                .i32_wrap_i64()
                .local_set(L_T1);

            // If `self_proof_view != u32::MAX`, prepend projected proof before `r0..r{argc-1}`!
            s.local_get(L_T1)
                .i32_const(-1)
                .i32_ne()
                .if_(BlockType::Empty)
                .local_get(L_V0)
                .local_get(L_T1)
                .call(rt.trait_proof)
                .local_set(L_V2)
                .i32_const(REG_PASS_BASE as i32)
                .local_get(L_V2)
                .i64_store(mem(0, 3));
            for i in 0..argc {
                s.i32_const(REG_PASS_BASE as i32)
                    .local_get(r_loc(Reg(i as u8)))
                    .i64_store(mem(8 + (i as u64) * 8, 3));
            }
            s.i32_const(0)
                .i32_const((argc + 1) as i32)
                .local_get(L_T0)
                .call_indirect(0, rt.nessa_fn_type)
                .local_set(r_loc(Reg(0)))
                .else_();
            for i in 0..argc {
                s.i32_const(REG_PASS_BASE as i32)
                    .local_get(r_loc(Reg(i as u8)))
                    .i64_store(mem((i as u64) * 8, 3));
            }
            s.i32_const(0)
                .i32_const(argc as i32)
                .local_get(L_T0)
                .call_indirect(0, rt.nessa_fn_type)
                .local_set(r_loc(Reg(0)))
                .end();
        }

        // ── Control Flow ────────────────────────────────────────────────
        Opcode::Jmp | Opcode::JmpFar => {
            let InstructionData::J { offset, .. } = ins.data else {
                unreachable!()
            };
            let target_pc = ((pc as i64) + (offset as i64)) as usize;
            let target_block = *pc_to_block.get(&target_pc).unwrap_or(&0);
            emit_jump_to_block(s, cur_block, target_block, dispatch_depth);
        }
        Opcode::JmpIf | Opcode::JmpIfNot => {
            let InstructionData::J { cond, offset } = ins.data else {
                unreachable!()
            };
            let target_pc = ((pc as i64) + (offset as i64)) as usize;
            let target_block = *pc_to_block.get(&target_pc).unwrap_or(&0);
            emit_is_truthy(s, cond, rt);
            if ins.opcode == Opcode::JmpIfNot {
                s.i32_eqz();
            }
            emit_cond_jump_to_block(s, cur_block, target_block, dispatch_depth);
        }
        Opcode::JmpIfNull | Opcode::JmpIfNotNull => {
            let InstructionData::J { cond, offset } = ins.data else {
                unreachable!()
            };
            let target_pc = ((pc as i64) + (offset as i64)) as usize;
            let target_block = *pc_to_block.get(&target_pc).unwrap_or(&0);
            s.local_get(r_loc(cond)).i64_const(VAL_NULL).i64_eq();
            if ins.opcode == Opcode::JmpIfNotNull {
                s.i32_eqz();
            }
            emit_cond_jump_to_block(s, cur_block, target_block, dispatch_depth);
        }

        // ── Calls & Returns ─────────────────────────────────────────────
        Opcode::Call | Opcode::CallFar => {
            let InstructionData::C { payload } = ins.data else {
                unreachable!()
            };
            let (arg_count, raw) = Instruction::c_arg_count_func_id(payload);
            let target_fid = if ins.opcode == Opcode::Call {
                raw
            } else {
                const_u32(&ctx.artifact.codegen_output.constants, raw).unwrap_or(0)
            };
            let argc = arg_count as usize;
            for i in 0..argc {
                s.i32_const(REG_PASS_BASE as i32)
                    .local_get(r_loc(Reg(i as u8)))
                    .i64_store(mem((i as u64) * 8, 3));
            }
            let wasm_callee = ctx
                .nessa_to_wasm_fn
                .get(target_fid as usize)
                .copied()
                .unwrap_or(rt.trap_unreachable);
            s.i32_const(0)
                .i32_const(argc as i32)
                .call(wasm_callee)
                .local_set(r_loc(Reg(0)));
        }
        Opcode::CallIndirect | Opcode::CallIndirectProof => {
            let InstructionData::C { payload } = ins.data else {
                unreachable!()
            };
            let (arg_count, closure_reg) = Instruction::c_call_indirect(payload);
            let argc = arg_count as usize;
            s.local_get(r_loc(closure_reg)).local_set(L_V0);
            for i in 0..argc {
                s.i32_const(REG_PASS_BASE as i32)
                    .local_get(r_loc(Reg(i as u8)))
                    .i64_store(mem((i as u64) * 8, 3));
            }
            // Check if `L_V0` is a Closure heap object (`type_index == TY_CLOSURE`), else dispatch `.apply`!
            s.local_get(L_V0)
                .call(rt.reflected_type)
                .local_tee(L_T2)
                .i32_const(TY_CLOSURE as i32)
                .i32_eq()
                .if_(BlockType::Empty)
                .local_get(L_V0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .local_get(L_T0)
                .i64_load(mem(0, 3))
                .i32_wrap_i64()
                .local_set(L_T1)
                .i32_const(argc as i32)
                .local_get(L_T1)
                .call_indirect(0, rt.nessa_fn_type)
                .local_set(r_loc(Reg(0)))
                .else_();
            // Non-closure callable (`List(i)` / `Map(k)` / `struct.apply(...)`)
            let apply_id = str_interner::intern("apply").as_u32();
            emit_method_dispatch(s, rt, ctx.info, L_V0, L_T2, apply_id, argc);
            s.end();
        }
        Opcode::CallMethod | Opcode::CallMethodFar => {
            let InstructionData::C { payload } = ins.data else {
                unreachable!()
            };
            let (arg_count, recv_reg, raw_mid) = Instruction::c_call_method(payload);
            let mid = if ins.opcode == Opcode::CallMethod {
                raw_mid
            } else {
                const_u32(&ctx.artifact.codegen_output.constants, raw_mid).unwrap_or(0)
            };
            let argc = arg_count as usize;
            s.local_get(r_loc(recv_reg)).local_set(L_V0);
            for i in 0..argc {
                s.i32_const(REG_PASS_BASE as i32)
                    .local_get(r_loc(Reg(i as u8)))
                    .i64_store(mem((i as u64) * 8, 3));
            }
            s.local_get(L_V0).call(rt.reflected_type).local_set(L_T2);
            emit_method_dispatch(s, rt, ctx.info, L_V0, L_T2, mid, argc);
        }
        Opcode::CallBuiltin => {
            let InstructionData::C { payload } = ins.data else {
                unreachable!()
            };
            let (arg_count, bid) = Instruction::c_arg_count_func_id(payload);
            let bname = ctx.builtin_names.get(&bid).copied().unwrap_or("");
            emit_builtin_call(ctx, s, bname, arg_count)?;
        }
        Opcode::Return => {
            let InstructionData::C { payload } = ins.data else {
                unreachable!()
            };
            let src = Instruction::c_return_reg(payload);
            s.local_get(L_SLOTS)
                .global_set(GLOBAL_SLOT_SP)
                .local_get(r_loc(src))
                .return_();
        }
        Opcode::ReturnUnit => {
            s.local_get(L_SLOTS)
                .global_set(GLOBAL_SLOT_SP)
                .i64_const(VAL_UNIT)
                .return_();
        }
        Opcode::Safepoint | Opcode::Nop => {}
        other => {
            return Err(WasmCompileError::new(format!(
                "unsupported opcode `{other:?}` in wasm codegen"
            )));
        }
    }
    Ok(())
}

fn retag_like_lhs(
    s: &mut wasm_encoder::InstructionSink<'_>,
    rt: &RtFuncIds,
    lhs_loc: u32,
    raw_int_loc: u32,
    dst_loc: u32,
) {
    s.local_get(lhs_loc)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .local_tee(L_V1)
        .i64_const(IMM_USIZE_TAG)
        .i64_eq()
        .if_(BlockType::Result(ValType::I64))
        .local_get(raw_int_loc)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_USIZE_TAG)
        .i64_or()
        .else_()
        .local_get(L_V1)
        .i64_const(IMM_ISIZE_TAG)
        .i64_eq()
        .if_(BlockType::Result(ValType::I64))
        .local_get(raw_int_loc)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_ISIZE_TAG)
        .i64_or()
        .else_()
        .local_get(L_V1)
        .i64_const(IMM_U_SMALL_TAG)
        .i64_eq()
        .if_(BlockType::Result(ValType::I64))
        .local_get(raw_int_loc)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_U_SMALL_TAG)
        .i64_or()
        .else_()
        .local_get(L_V1)
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .if_(BlockType::Result(ValType::I64))
        .local_get(lhs_loc)
        .i64_const(0x0000_0780_0000_0000)
        .i64_and()
        .local_get(raw_int_loc)
        .i64_const(0xffff_ffff)
        .i64_and()
        .i64_const(7)
        .i64_shl()
        .i64_or()
        .i64_const(IMM_NARROW_TAG)
        .i64_or()
        .else_()
        .local_get(raw_int_loc)
        .call(rt.from_i64)
        .end()
        .end()
        .end()
        .end()
        .local_set(dst_loc);
}

fn emit_method_dispatch(
    s: &mut wasm_encoder::InstructionSink<'_>,
    rt: &RtFuncIds,
    info: &StaticLayoutInfo,
    recv_val_loc: u32,
    recv_ty_loc: u32,
    method_str_id: u32,
    argc: usize,
) {
    // Collection `apply` / `update` fast check (`List` or `Map`), matching `exec_collection_call`!
    let apply_id = str_interner::intern("apply").as_u32();
    let update_id = str_interner::intern("update").as_u32();
    if method_str_id == apply_id && argc == 1 {
        s.local_get(recv_ty_loc)
            .i32_const(info.list_type_idx as i32)
            .i32_eq()
            .local_get(recv_ty_loc)
            .i32_const(info.map_type_idx as i32)
            .i32_eq()
            .i32_or()
            .if_(BlockType::Empty)
            .local_get(recv_val_loc)
            .i32_const(REG_PASS_BASE as i32)
            .i64_load(mem(0, 3))
            .call(rt.list_get)
            .local_set(r_loc(Reg(0)))
            .else_();
    } else if method_str_id == update_id && argc == 2 {
        s.local_get(recv_ty_loc)
            .i32_const(info.list_type_idx as i32)
            .i32_eq()
            .local_get(recv_ty_loc)
            .i32_const(info.map_type_idx as i32)
            .i32_eq()
            .i32_or()
            .if_(BlockType::Empty)
            .local_get(recv_val_loc)
            .i32_const(REG_PASS_BASE as i32)
            .i64_load(mem(0, 3))
            .i32_const(REG_PASS_BASE as i32)
            .i64_load(mem(8, 3))
            .call(rt.list_set)
            .i64_const(VAL_UNIT)
            .local_set(r_loc(Reg(0)))
            .else_();
    }

    // Lookup `(self_proof_view << 32) | func_id` in static Method Dispatch Table
    s.local_get(recv_ty_loc)
        .i32_const(method_str_id as i32)
        .call(rt.find_method)
        .local_tee(L_V1)
        .i64_const(-1)
        .i64_eq()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(L_V1)
        .i32_wrap_i64()
        .local_set(L_T0)
        .local_get(L_V1)
        .i64_const(32)
        .i64_shr_u()
        .i32_wrap_i64()
        .local_set(L_T1);

    // Shift explicit args right by 1 (or 2 if `self_proof_view != u32::MAX`)
    s.local_get(L_T1)
        .i32_const(-1)
        .i32_ne()
        .if_(BlockType::Empty);
    for i in (0..argc).rev() {
        s.i32_const(REG_PASS_BASE as i32)
            .i32_const(REG_PASS_BASE as i32)
            .i64_load(mem((i as u64) * 8, 3))
            .i64_store(mem(16 + (i as u64) * 8, 3));
    }
    s.local_get(recv_val_loc)
        .local_get(L_T1)
        .call(rt.trait_proof)
        .local_set(L_V2)
        .i32_const(REG_PASS_BASE as i32)
        .local_get(L_V2)
        .i64_store(mem(0, 3))
        .i32_const(REG_PASS_BASE as i32)
        .local_get(recv_val_loc)
        .i64_store(mem(8, 3))
        .i32_const(0)
        .i32_const((argc + 2) as i32)
        .local_get(L_T0)
        .call_indirect(0, rt.nessa_fn_type)
        .local_set(r_loc(Reg(0)))
        .else_();
    for i in (0..argc).rev() {
        s.i32_const(REG_PASS_BASE as i32)
            .i32_const(REG_PASS_BASE as i32)
            .i64_load(mem((i as u64) * 8, 3))
            .i64_store(mem(8 + (i as u64) * 8, 3));
    }
    s.i32_const(REG_PASS_BASE as i32)
        .local_get(recv_val_loc)
        .i64_store(mem(0, 3))
        .i32_const(0)
        .i32_const((argc + 1) as i32)
        .local_get(L_T0)
        .call_indirect(0, rt.nessa_fn_type)
        .local_set(r_loc(Reg(0)))
        .end();

    if (method_str_id == apply_id && argc == 1) || (method_str_id == update_id && argc == 2) {
        s.end();
    }
}

// ── Builtin Lowering ────────────────────────────────────────────────────────

fn emit_builtin_call(
    ctx: &LowerCtx<'_>,
    s: &mut wasm_encoder::InstructionSink<'_>,
    name: &str,
    _argc: u8,
) -> Result<(), WasmCompileError> {
    let rt = ctx.rt;
    let r0 = r_loc(Reg(0));
    let r1 = r_loc(Reg(1));
    let r2 = r_loc(Reg(2));

    match name {
        // I/O
        "print" => {
            s.i32_const(1)
                .local_get(r0)
                .call(rt.to_string)
                .i32_const(0)
                .call(rt.write_str)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "println" => {
            s.i32_const(1)
                .local_get(r0)
                .call(rt.to_string)
                .i32_const(1)
                .call(rt.write_str)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "eprint" => {
            s.i32_const(2)
                .local_get(r0)
                .call(rt.to_string)
                .i32_const(0)
                .call(rt.write_str)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "eprintln" => {
            s.i32_const(2)
                .local_get(r0)
                .call(rt.to_string)
                .i32_const(1)
                .call(rt.write_str)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "__io_flush_stdout" | "__io_flush_stderr" => {
            // Return `(0, "")` 2-tuple
            let pair_ty = (0..ctx.info.type_count)
                .find(|&idx| {
                    matches!(
                        &ctx.artifact.type_pool.get(TypeIndex::from_raw(idx)).kind,
                        TypeKind::Tuple { elements } if elements.len() == 2
                    )
                })
                .unwrap_or(TY_I64);
            s.i32_const(FMT_SCRATCH_ADDR)
                .i32_const(0)
                .call(rt.alloc_str)
                .local_set(L_V0)
                .i32_const(pair_ty as i32)
                .i32_const(ROLE_ORDINARY)
                .i32_const(2)
                .i32_const(0)
                .call(rt.alloc_obj)
                .local_tee(L_T0)
                .i64_const(IMM_I_SMALL_TAG)
                .i64_store(mem(0, 3))
                .local_get(L_T0)
                .local_get(L_V0)
                .i64_store(mem(8, 3))
                .local_get(L_T0)
                .i64_extend_i32_u()
                .local_set(r0);
        }
        "__file_is_open" => {
            s.i64_const(VAL_FALSE).local_set(r0);
        }

        // Conversions & reflection
        "to_string" | "__display_quote" => {
            s.local_get(r0).call(rt.to_string).local_set(r0);
        }
        "__display_enter_tuple" | "__display_exit_tuple" => {
            s.i64_const(VAL_UNIT).local_set(r0);
        }
        "to_i64" => {
            s.local_get(r0)
                .call(rt.to_i64)
                .call(rt.from_i64)
                .local_set(r0);
        }
        "to_f64" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .call(rt.from_f64)
                .local_set(r0);
        }
        "type_of" => {
            s.local_get(r0)
                .call(rt.reflected_type)
                .i64_extend_i32_u()
                .i64_const(7)
                .i64_shl()
                .i64_const(IMM_TYPE_TAG)
                .i64_or()
                .local_set(r0);
        }
        "__scalar_eq" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.val_eq)
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }
        "__native_equal" => {
            // If either operand is immediate or string, return bool; else return null so std uses PartialEq
            s.local_get(r0)
                .i64_const(TAG_MASK)
                .i64_and()
                .i64_const(0)
                .i64_ne()
                .local_get(r1)
                .i64_const(TAG_MASK)
                .i64_and()
                .i64_const(0)
                .i64_ne()
                .i32_or()
                .if_(BlockType::Result(ValType::I64))
                .local_get(r0)
                .local_get(r1)
                .call(rt.val_eq)
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .else_()
                .local_get(r0)
                .i32_wrap_i64()
                .i32_const(12)
                .i32_sub()
                .i32_load(mem(0, 2))
                .i32_const(ROLE_STRING_UTF8)
                .i32_eq()
                .if_(BlockType::Result(ValType::I64))
                .local_get(r0)
                .local_get(r1)
                .call(rt.val_eq)
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .else_()
                .i64_const(VAL_NULL)
                .end()
                .end()
                .local_set(r0);
        }
        "__scalar_cmp" => {
            // Returns `-1`, `0`, `1` as `i64` (or `null` for NaN!)
            s.local_get(r0)
                .local_get(r1)
                .call(rt.val_eq)
                .if_(BlockType::Result(ValType::I64))
                .i64_const(IMM_I_SMALL_TAG) // 0
                .else_()
                .local_get(r0)
                .local_get(r1)
                .i32_const(2) // Lt
                .call(rt.cmp)
                .i64_const(VAL_TRUE)
                .i64_eq()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(encode_i64_imm(-1).unwrap() as i64)
                .else_()
                .local_get(r0)
                .local_get(r1)
                .i32_const(4) // Gt
                .call(rt.cmp)
                .i64_const(VAL_TRUE)
                .i64_eq()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(encode_i64_imm(1).unwrap() as i64)
                .else_()
                .i64_const(VAL_NULL)
                .end()
                .end()
                .end()
                .local_set(r0);
        }
        "__iteration_step_done" => {
            // `IterationStep.done` has variant tag == `ITERATION_DONE_TAG` (0) as an inline `IMM_ENUM`!
            s.local_get(r0)
                .i64_const(IMM_LOW7_MASK)
                .i64_and()
                .i64_const(IMM_ENUM_TAG)
                .i64_eq()
                .local_get(r0)
                .i64_const(7)
                .i64_shr_u()
                .i64_const((1 << 25) - 1)
                .i64_and()
                .i64_const(type_pool::ITERATION_DONE_TAG as i64)
                .i64_eq()
                .i32_and()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }
        "__tuple_len" => {
            s.local_get(r0)
                .i64_const(TAG_MASK)
                .i64_and()
                .i64_eqz()
                .local_get(r0)
                .i64_const(0)
                .i64_ne()
                .i32_and()
                .if_(BlockType::Result(ValType::I64))
                .local_get(r0)
                .call(rt.reflected_type)
                .i32_const(5)
                .i32_shl()
                .i32_const(ctx.info.type_meta_addr as i32)
                .i32_add()
                .local_tee(L_T0)
                .i32_load(mem(4, 2))
                .i32_const(TK_TUPLE as i32)
                .i32_eq()
                .if_(BlockType::Result(ValType::I64))
                .local_get(L_T0)
                .i32_load(mem(8, 2))
                .i64_extend_i32_u()
                .call(rt.from_i64)
                .else_()
                .i64_const(VAL_NULL)
                .end()
                .else_()
                .i64_const(VAL_NULL)
                .end()
                .local_set(r0);
        }
        "__tuple_get" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .local_get(r1)
                .call(rt.to_i64)
                .i32_wrap_i64()
                .i32_const(3)
                .i32_shl()
                .i32_add()
                .i64_load(mem(0, 3))
                .local_set(r0);
        }
        "__scalar_hash" => {
            // Coherent 64-bit hash across String (FNV-1a), Unit/null/bool/char, and numbers (canonicalized via f64).
            s.local_get(r0)
                .i64_const(TAG_MASK)
                .i64_and()
                .i64_eqz()
                .local_get(r0)
                .i64_const(0)
                .i64_ne()
                .i32_and()
                .if_(BlockType::Empty)
                .local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(16)
                .i32_sub()
                .i32_load(mem(0, 2))
                .i32_const(TY_STR as i32)
                .i32_eq()
                .if_(BlockType::Empty)
                // FNV-1a over UTF-8 bytes `[L_T0 .. L_T0 + len]`
                .local_get(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1)
                .i32_const(0)
                .local_set(L_T2)
                .i64_const(0xcbf2_9ce4_8422_2325_u64 as i64)
                .local_set(L_V0)
                .block(BlockType::Empty)
                .loop_(BlockType::Empty)
                .local_get(L_T2)
                .local_get(L_T1)
                .i32_ge_u()
                .br_if(1)
                .local_get(L_V0)
                .local_get(L_T0)
                .local_get(L_T2)
                .i32_add()
                .i64_load8_u(mem(0, 0))
                .i64_xor()
                .i64_const(0x0100_0000_01b3_i64)
                .i64_mul()
                .local_set(L_V0)
                .local_get(L_T2)
                .i32_const(1)
                .i32_add()
                .local_set(L_T2)
                .br(0)
                .end()
                .end()
                .else_()
                .local_get(r0)
                .call(rt.to_f64)
                .local_tee(L_F0)
                .f64_const(f64c(0.0))
                .f64_eq()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(0)
                .else_()
                .local_get(L_F0)
                .i64_reinterpret_f64()
                .end()
                .local_set(L_V0)
                .end()
                .else_()
                .local_get(r0)
                .i64_const(IMM_LOW7_MASK)
                .i64_and()
                .local_tee(L_V1)
                .i64_const(0x06)
                .i64_eq()
                .local_get(L_V1)
                .i64_const(0x16)
                .i64_eq()
                .i32_or()
                .local_get(L_V1)
                .i64_const(0x26)
                .i64_eq()
                .i32_or()
                .local_get(L_V1)
                .i64_const(0x36)
                .i64_eq()
                .i32_or()
                .local_get(L_V1)
                .i64_const(IMM_CHAR_TAG)
                .i64_eq()
                .i32_or()
                .local_get(L_V1)
                .i64_const(IMM_ENUM_TAG)
                .i64_eq()
                .i32_or()
                .if_(BlockType::Empty)
                .local_get(r0)
                .local_set(L_V0)
                .else_()
                .local_get(r0)
                .call(rt.to_f64)
                .local_tee(L_F0)
                .f64_const(f64c(0.0))
                .f64_eq()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(0)
                .else_()
                .local_get(L_F0)
                .i64_reinterpret_f64()
                .end()
                .local_set(L_V0)
                .end()
                .end()
                // SplitMix64 finalizer -> 57-bit U64 immediate
                .local_get(L_V0)
                .local_get(L_V0)
                .i64_const(30)
                .i64_shr_u()
                .i64_xor()
                .i64_const(0xbf58_476d_1ce4_e5b9_u64 as i64)
                .i64_mul()
                .local_tee(L_V0)
                .local_get(L_V0)
                .i64_const(27)
                .i64_shr_u()
                .i64_xor()
                .i64_const(0x94d0_49bb_1331_11eb_u64 as i64)
                .i64_mul()
                .local_tee(L_V0)
                .local_get(L_V0)
                .i64_const(31)
                .i64_shr_u()
                .i64_xor()
                .i64_const(7)
                .i64_shr_u()
                .i64_const(7)
                .i64_shl()
                .i64_const(IMM_U_SMALL_TAG)
                .i64_or()
                .local_set(r0);
        }
        "__env_args" => {
            s.i32_const(0).call(rt.list_new).local_set(L_V0);
            if let (Some(fn_sizes), Some(fn_get)) =
                (ctx.info.wasi_args_sizes_get, ctx.info.wasi_args_get)
            {
                // Use `FMT_SCRATCH_ADDR` (`+0`: argc, `+4`: argv_buf_size)
                s.i32_const(FMT_SCRATCH_ADDR)
                    .i32_const(FMT_SCRATCH_ADDR + 4)
                    .call(fn_sizes)
                    .i32_eqz()
                    .if_(BlockType::Empty)
                    .i32_const(FMT_SCRATCH_ADDR)
                    .i32_load(mem(0, 2))
                    .local_set(L_T0) // argc
                    .i32_const(FMT_SCRATCH_ADDR + 4)
                    .i32_load(mem(0, 2))
                    .local_set(L_T1) // buf_size
                    .local_get(L_T0)
                    .i32_const(2)
                    .i32_shl()
                    .call(rt.alloc)
                    .local_set(L_T2) // argv_ptrs
                    .local_get(L_T1)
                    .call(rt.alloc)
                    .local_set(L_T3) // argv_buf
                    .local_get(L_T2)
                    .local_get(L_T3)
                    .call(fn_get)
                    .i32_eqz()
                    .if_(BlockType::Empty)
                    .i64_const(0)
                    .local_set(L_V1) // i
                    .block(BlockType::Empty)
                    .loop_(BlockType::Empty)
                    .local_get(L_V1)
                    .i32_wrap_i64()
                    .local_get(L_T0)
                    .i32_ge_u()
                    .br_if(1)
                    .local_get(L_T2)
                    .local_get(L_V1)
                    .i32_wrap_i64()
                    .i32_const(2)
                    .i32_shl()
                    .i32_add()
                    .i32_load(mem(0, 2))
                    .local_set(L_T1) // arg_cstr_ptr
                    // Measure NUL-terminated length -> L_T3
                    .i32_const(0)
                    .local_set(L_T3)
                    .block(BlockType::Empty)
                    .loop_(BlockType::Empty)
                    .local_get(L_T1)
                    .local_get(L_T3)
                    .i32_add()
                    .i32_load8_u(mem(0, 0))
                    .i32_eqz()
                    .br_if(1)
                    .local_get(L_T3)
                    .i32_const(1)
                    .i32_add()
                    .local_set(L_T3)
                    .br(0)
                    .end()
                    .end()
                    .local_get(L_V0)
                    .local_get(L_T1)
                    .local_get(L_T3)
                    .call(rt.alloc_str)
                    .call(rt.list_push)
                    .local_get(L_V1)
                    .i64_const(1)
                    .i64_add()
                    .local_set(L_V1)
                    .br(0)
                    .end()
                    .end()
                    .end()
                    .end();
            }
            s.local_get(L_V0).local_set(r0);
        }

        // Strings & Chars
        "str_len" | "str_char_len" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .i64_extend_i32_u()
                .call(rt.from_i64)
                .local_set(r0);
        }
        "str_concat" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.str_concat)
                .local_set(r0);
        }
        "str_is_empty" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .i32_eqz()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }
        "str_byte_at" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1)
                .local_get(r1)
                .call(rt.to_i64)
                .i32_wrap_i64()
                .local_tee(L_T2)
                .local_get(L_T1)
                .i32_ge_u()
                .if_(BlockType::Empty)
                .unreachable()
                .end()
                .local_get(L_T0)
                .local_get(L_T2)
                .i32_add()
                .i32_load8_u(mem(0, 0))
                .i64_extend_i32_u()
                .call(rt.from_i64)
                .local_set(r0);
        }
        "str_char_at" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1)
                .local_get(r1)
                .call(rt.to_i64)
                .i32_wrap_i64()
                .local_tee(L_T2)
                .local_get(L_T1)
                .i32_ge_u()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_NULL)
                .else_()
                .local_get(L_T0)
                .local_get(L_T2)
                .i32_add()
                .i32_load8_u(mem(0, 0))
                .i64_extend_i32_u()
                .i64_const(7)
                .i64_shl()
                .i64_const(IMM_CHAR_TAG)
                .i64_or()
                .end()
                .local_set(r0);
        }
        "str_slice" | "str_byte_slice" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1)
                .local_get(r1)
                .call(rt.to_i64)
                .i32_wrap_i64()
                .local_set(L_T2)
                .local_get(r2)
                .call(rt.to_i64)
                .i32_wrap_i64()
                .local_tee(L_T3)
                .local_get(L_T1)
                .i32_gt_u()
                .local_get(L_T2)
                .local_get(L_T3)
                .i32_gt_u()
                .i32_or()
                .if_(BlockType::Empty)
                .unreachable()
                .end()
                .local_get(L_T0)
                .local_get(L_T2)
                .i32_add()
                .local_get(L_T3)
                .local_get(L_T2)
                .i32_sub()
                .call(rt.alloc_str)
                .local_set(r0);
        }
        "str_starts_with" | "str_ends_with" | "str_contains" => {
            // Byte substring search
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1) // hay_len
                .local_get(r1)
                .i32_wrap_i64()
                .local_tee(L_T2)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T3); // needle_len
            if name == "str_starts_with" {
                s.local_get(L_T3)
                    .local_get(L_T1)
                    .i32_gt_u()
                    .if_(BlockType::Result(ValType::I64))
                    .i64_const(VAL_FALSE)
                    .else_()
                    .local_get(L_T0)
                    .local_get(L_T3)
                    .call(rt.alloc_str)
                    .local_get(r1)
                    .call(rt.str_eq)
                    .if_(BlockType::Result(ValType::I64))
                    .i64_const(VAL_TRUE)
                    .else_()
                    .i64_const(VAL_FALSE)
                    .end()
                    .end()
                    .local_set(r0);
            } else if name == "str_ends_with" {
                s.local_get(L_T3)
                    .local_get(L_T1)
                    .i32_gt_u()
                    .if_(BlockType::Result(ValType::I64))
                    .i64_const(VAL_FALSE)
                    .else_()
                    .local_get(L_T0)
                    .local_get(L_T1)
                    .local_get(L_T3)
                    .i32_sub()
                    .i32_add()
                    .local_get(L_T3)
                    .call(rt.alloc_str)
                    .local_get(r1)
                    .call(rt.str_eq)
                    .if_(BlockType::Result(ValType::I64))
                    .i64_const(VAL_TRUE)
                    .else_()
                    .i64_const(VAL_FALSE)
                    .end()
                    .end()
                    .local_set(r0);
            } else {
                // str_contains
                s.local_get(L_T3)
                    .local_get(L_T1)
                    .i32_gt_u()
                    .if_(BlockType::Empty)
                    .i64_const(VAL_FALSE)
                    .local_set(r0)
                    .else_()
                    .i64_const(0)
                    .local_set(L_V0) // pos
                    .i64_const(VAL_FALSE)
                    .local_set(r0)
                    .block(BlockType::Empty)
                    .loop_(BlockType::Empty)
                    .local_get(L_V0)
                    .i32_wrap_i64()
                    .local_get(L_T3)
                    .i32_add()
                    .local_get(L_T1)
                    .i32_gt_u()
                    .br_if(1)
                    .local_get(L_T0)
                    .local_get(L_V0)
                    .i32_wrap_i64()
                    .i32_add()
                    .local_get(L_T3)
                    .call(rt.alloc_str)
                    .local_get(r1)
                    .call(rt.str_eq)
                    .if_(BlockType::Empty)
                    .i64_const(VAL_TRUE)
                    .local_set(r0)
                    .br(2)
                    .end()
                    .local_get(L_V0)
                    .i64_const(1)
                    .i64_add()
                    .local_set(L_V0)
                    .br(0)
                    .end()
                    .end()
                    .end();
            }
        }
        "str_trim" | "str_trim_ascii" | "str_trim_start" | "str_trim_end" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1) // end
                .i32_const(0)
                .local_set(L_T2); // start
            if name != "str_trim_end" {
                s.block(BlockType::Empty)
                    .loop_(BlockType::Empty)
                    .local_get(L_T2)
                    .local_get(L_T1)
                    .i32_ge_u()
                    .br_if(1)
                    .local_get(L_T0)
                    .local_get(L_T2)
                    .i32_add()
                    .i32_load8_u(mem(0, 0))
                    .i32_const(32)
                    .i32_gt_u()
                    .br_if(1)
                    .local_get(L_T2)
                    .i32_const(1)
                    .i32_add()
                    .local_set(L_T2)
                    .br(0)
                    .end()
                    .end();
            }
            if name != "str_trim_start" {
                s.block(BlockType::Empty)
                    .loop_(BlockType::Empty)
                    .local_get(L_T1)
                    .local_get(L_T2)
                    .i32_le_u()
                    .br_if(1)
                    .local_get(L_T0)
                    .local_get(L_T1)
                    .i32_add()
                    .i32_const(1)
                    .i32_sub()
                    .i32_load8_u(mem(0, 0))
                    .i32_const(32)
                    .i32_gt_u()
                    .br_if(1)
                    .local_get(L_T1)
                    .i32_const(1)
                    .i32_sub()
                    .local_set(L_T1)
                    .br(0)
                    .end()
                    .end();
            }
            s.local_get(L_T0)
                .local_get(L_T2)
                .i32_add()
                .local_get(L_T1)
                .local_get(L_T2)
                .i32_sub()
                .call(rt.alloc_str)
                .local_set(r0);
        }
        "str_to_upper" | "str_to_ascii_uppercase" | "str_to_lower" | "str_to_ascii_lowercase" => {
            let to_upper = name.contains("upper");
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1)
                .local_get(L_T0)
                .local_get(L_T1)
                .call(rt.alloc_str)
                .local_tee(r0)
                .i32_wrap_i64()
                .local_set(L_T2)
                .i32_const(0)
                .local_set(L_T3)
                .block(BlockType::Empty)
                .loop_(BlockType::Empty)
                .local_get(L_T3)
                .local_get(L_T1)
                .i32_ge_u()
                .br_if(1)
                .local_get(L_T2)
                .local_get(L_T3)
                .i32_add()
                .i32_load8_u(mem(0, 0))
                .local_set(L_T0);
            if to_upper {
                s.local_get(L_T0)
                    .i32_const(b'a' as i32)
                    .i32_ge_u()
                    .local_get(L_T0)
                    .i32_const(b'z' as i32)
                    .i32_le_u()
                    .i32_and()
                    .if_(BlockType::Empty)
                    .local_get(L_T2)
                    .local_get(L_T3)
                    .i32_add()
                    .local_get(L_T0)
                    .i32_const(32)
                    .i32_sub()
                    .i32_store8(mem(0, 0))
                    .end();
            } else {
                s.local_get(L_T0)
                    .i32_const(b'A' as i32)
                    .i32_ge_u()
                    .local_get(L_T0)
                    .i32_const(b'Z' as i32)
                    .i32_le_u()
                    .i32_and()
                    .if_(BlockType::Empty)
                    .local_get(L_T2)
                    .local_get(L_T3)
                    .i32_add()
                    .local_get(L_T0)
                    .i32_const(32)
                    .i32_add()
                    .i32_store8(mem(0, 0))
                    .end();
            }
            s.local_get(L_T3)
                .i32_const(1)
                .i32_add()
                .local_set(L_T3)
                .br(0)
                .end()
                .end();
        }
        "str_repeat" => {
            s.local_get(r1)
                .call(rt.to_i64)
                .i32_wrap_i64()
                .local_set(L_T1) // count
                .i32_const(FMT_SCRATCH_ADDR)
                .i32_const(0)
                .call(rt.alloc_str)
                .local_set(L_V0)
                .i32_const(0)
                .local_set(L_T2)
                .block(BlockType::Empty)
                .loop_(BlockType::Empty)
                .local_get(L_T2)
                .local_get(L_T1)
                .i32_ge_s()
                .br_if(1)
                .local_get(L_V0)
                .local_get(r0)
                .call(rt.str_concat)
                .local_set(L_V0)
                .local_get(L_T2)
                .i32_const(1)
                .i32_add()
                .local_set(L_T2)
                .br(0)
                .end()
                .end()
                .local_get(L_V0)
                .local_set(r0);
        }
        "str_is_ascii" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_const(8)
                .i32_sub()
                .i32_load(mem(0, 2))
                .local_set(L_T1)
                .i32_const(0)
                .local_set(L_T2)
                .i64_const(VAL_TRUE)
                .local_set(r0)
                .block(BlockType::Empty)
                .loop_(BlockType::Empty)
                .local_get(L_T2)
                .local_get(L_T1)
                .i32_ge_u()
                .br_if(1)
                .local_get(L_T0)
                .local_get(L_T2)
                .i32_add()
                .i32_load8_u(mem(0, 0))
                .i32_const(0x80)
                .i32_ge_u()
                .if_(BlockType::Empty)
                .i64_const(VAL_FALSE)
                .local_set(r0)
                .br(2)
                .end()
                .local_get(L_T2)
                .i32_const(1)
                .i32_add()
                .local_set(L_T2)
                .br(0)
                .end()
                .end();
        }
        "char_to_code" => {
            s.local_get(r0)
                .i64_const(7)
                .i64_shr_u()
                .call(rt.from_i64)
                .local_set(r0);
        }
        "char_from_code" => {
            s.local_get(r0)
                .call(rt.to_i64)
                .local_tee(L_V0)
                .i64_const(0x10FFFF)
                .i64_gt_u()
                .local_get(L_V0)
                .i64_const(0xD800)
                .i64_ge_u()
                .local_get(L_V0)
                .i64_const(0xDFFF)
                .i64_le_u()
                .i32_and()
                .i32_or()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_NULL)
                .else_()
                .local_get(L_V0)
                .i64_const(7)
                .i64_shl()
                .i64_const(IMM_CHAR_TAG)
                .i64_or()
                .end()
                .local_set(r0);
        }
        "char_into_string" => {
            s.local_get(r0).call(rt.to_string).local_set(r0);
        }
        "char_is_ascii_digit" | "char_is_numeric" => {
            s.local_get(r0)
                .i64_const(7)
                .i64_shr_u()
                .local_tee(L_V0)
                .i64_const(b'0' as i64)
                .i64_ge_u()
                .local_get(L_V0)
                .i64_const(b'9' as i64)
                .i64_le_u()
                .i32_and()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }
        "char_is_whitespace" => {
            s.local_get(r0)
                .i64_const(7)
                .i64_shr_u()
                .local_tee(L_V0)
                .i64_const(32)
                .i64_eq()
                .local_get(L_V0)
                .i64_const(9)
                .i64_ge_u()
                .local_get(L_V0)
                .i64_const(13)
                .i64_le_u()
                .i32_and()
                .i32_or()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }
        "char_is_ascii"
        | "char_is_alphabetic"
        | "char_is_alphanumeric"
        | "char_is_uppercase"
        | "char_is_lowercase"
        | "char_is_ascii_alphabetic"
        | "char_is_ascii_hexdigit" => {
            s.local_get(r0).i64_const(7).i64_shr_u().local_set(L_V0);
            match name {
                "char_is_ascii" => {
                    s.local_get(L_V0).i64_const(0x80).i64_lt_u();
                }
                "char_is_uppercase" => {
                    s.local_get(L_V0)
                        .i64_const(b'A' as i64)
                        .i64_ge_u()
                        .local_get(L_V0)
                        .i64_const(b'Z' as i64)
                        .i64_le_u()
                        .i32_and();
                }
                "char_is_lowercase" => {
                    s.local_get(L_V0)
                        .i64_const(b'a' as i64)
                        .i64_ge_u()
                        .local_get(L_V0)
                        .i64_const(b'z' as i64)
                        .i64_le_u()
                        .i32_and();
                }
                _ => {
                    s.local_get(L_V0)
                        .i64_const(b'A' as i64)
                        .i64_ge_u()
                        .local_get(L_V0)
                        .i64_const(b'Z' as i64)
                        .i64_le_u()
                        .i32_and()
                        .local_get(L_V0)
                        .i64_const(b'a' as i64)
                        .i64_ge_u()
                        .local_get(L_V0)
                        .i64_const(b'z' as i64)
                        .i64_le_u()
                        .i32_and()
                        .i32_or();
                    if name == "char_is_alphanumeric" || name == "char_is_ascii_hexdigit" {
                        s.local_get(L_V0)
                            .i64_const(b'0' as i64)
                            .i64_ge_u()
                            .local_get(L_V0)
                            .i64_const(b'9' as i64)
                            .i64_le_u()
                            .i32_and()
                            .i32_or();
                    }
                }
            }
            s.if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }
        "char_to_ascii_uppercase" => {
            s.local_get(r0)
                .i64_const(7)
                .i64_shr_u()
                .local_tee(L_V0)
                .i64_const(b'a' as i64)
                .i64_ge_u()
                .local_get(L_V0)
                .i64_const(b'z' as i64)
                .i64_le_u()
                .i32_and()
                .if_(BlockType::Empty)
                .local_get(L_V0)
                .i64_const(32)
                .i64_sub()
                .i64_const(7)
                .i64_shl()
                .i64_const(IMM_CHAR_TAG)
                .i64_or()
                .local_set(r0)
                .end();
        }
        "char_to_ascii_lowercase" => {
            s.local_get(r0)
                .i64_const(7)
                .i64_shr_u()
                .local_tee(L_V0)
                .i64_const(b'A' as i64)
                .i64_ge_u()
                .local_get(L_V0)
                .i64_const(b'Z' as i64)
                .i64_le_u()
                .i32_and()
                .if_(BlockType::Empty)
                .local_get(L_V0)
                .i64_const(32)
                .i64_add()
                .i64_const(7)
                .i64_shl()
                .i64_const(IMM_CHAR_TAG)
                .i64_or()
                .local_set(r0)
                .end();
        }

        // Lists, Maps, Bytes
        "__list_init" => {
            s.i32_const(0).call(rt.list_new).local_set(r0);
        }
        "__list_len" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .i32_load(mem(0, 2))
                .i64_extend_i32_u()
                .call(rt.from_i64)
                .local_set(r0);
        }
        "__list_get" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.list_get)
                .local_set(r0);
        }
        "__list_set" => {
            s.local_get(r0)
                .local_get(r1)
                .local_get(r2)
                .call(rt.list_set)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "__list_push" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.list_push)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "__list_pop" => {
            s.local_get(r0).call(rt.list_pop).local_set(r0);
        }
        "__list_clear" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .i32_const(0)
                .i32_store(mem(0, 2))
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "__list_clone" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .local_tee(L_T0)
                .i32_load(mem(0, 2))
                .local_tee(L_T1)
                .call(rt.list_new)
                .local_tee(L_V0)
                .i32_wrap_i64()
                .i32_load(mem(8, 2))
                .local_get(L_T0)
                .i32_load(mem(8, 2))
                .local_get(L_T1)
                .i32_const(3)
                .i32_shl()
                .memory_copy(0, 0)
                .local_get(L_V0)
                .local_set(r0);
        }
        "__map_init" => {
            s.i32_const(8).call(rt.map_new).local_set(r0);
        }
        "__map_len" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .i32_load(mem(0, 2))
                .i64_extend_i32_u()
                .call(rt.from_i64)
                .local_set(r0);
        }
        "__map_get" => {
            s.local_get(r0).local_get(r1).call(rt.map_get).local_set(r0);
        }
        "__map_set" => {
            s.local_get(r0)
                .local_get(r1)
                .local_get(r2)
                .call(rt.map_set)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "__map_contains" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.map_contains)
                .local_set(r0);
        }
        "__map_remove" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.map_remove)
                .local_set(r0);
        }
        "__map_keys" => {
            s.local_get(r0).call(rt.map_keys).local_set(r0);
        }
        "__bytes_new" | "__bytes_with_capacity" => {
            if name == "__bytes_new" {
                s.local_get(r0)
                    .call(rt.to_i64)
                    .i32_wrap_i64()
                    .call(rt.bytes_new)
                    .local_set(r0);
            } else {
                s.i32_const(0).call(rt.bytes_new).local_set(r0);
            }
        }
        "__bytes_len" => {
            s.local_get(r0)
                .i32_wrap_i64()
                .i32_load(mem(4, 2))
                .i64_extend_i32_u()
                .call(rt.from_i64)
                .local_set(r0);
        }
        "__bytes_get" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.bytes_get)
                .local_set(r0);
        }
        "__bytes_set" => {
            s.local_get(r0)
                .local_get(r1)
                .local_get(r2)
                .call(rt.bytes_set)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "__bytes_push" => {
            s.local_get(r0)
                .local_get(r1)
                .call(rt.bytes_push)
                .i64_const(VAL_UNIT)
                .local_set(r0);
        }
        "__bytes_from_string" => {
            s.local_get(r0).call(rt.bytes_from_str).local_set(r0);
        }
        "__bytes_to_string_lossy" => {
            s.local_get(r0).call(rt.bytes_to_str).local_set(r0);
        }
        "__bytes_eq" => {
            s.local_get(r0)
                .call(rt.bytes_to_str)
                .local_get(r1)
                .call(rt.bytes_to_str)
                .call(rt.str_eq)
                .if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }

        // Math constants
        "pi" => emit_from_f64_inline_const(s, std::f64::consts::PI, r0),
        "e" => emit_from_f64_inline_const(s, std::f64::consts::E, r0),
        "tau" => emit_from_f64_inline_const(s, std::f64::consts::TAU, r0),
        "ln_2" => emit_from_f64_inline_const(s, std::f64::consts::LN_2, r0),
        "ln_10" => emit_from_f64_inline_const(s, std::f64::consts::LN_10, r0),
        "log2_e" => emit_from_f64_inline_const(s, std::f64::consts::LOG2_E, r0),
        "log10_e" => emit_from_f64_inline_const(s, std::f64::consts::LOG10_E, r0),
        "sqrt_2" => emit_from_f64_inline_const(s, std::f64::consts::SQRT_2, r0),
        "infinity" => emit_from_f64_inline_const(s, f64::INFINITY, r0),
        "neg_infinity" => emit_from_f64_inline_const(s, f64::NEG_INFINITY, r0),
        "nan" => emit_from_f64_inline_const(s, f64::NAN, r0),
        "epsilon" => emit_from_f64_inline_const(s, f64::EPSILON, r0),
        "min_positive" => emit_from_f64_inline_const(s, f64::MIN_POSITIVE, r0),
        "frac_pi_2" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_PI_2, r0),
        "frac_pi_3" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_PI_3, r0),
        "frac_pi_4" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_PI_4, r0),
        "frac_pi_6" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_PI_6, r0),
        "frac_pi_8" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_PI_8, r0),
        "frac_1_pi" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_1_PI, r0),
        "frac_2_pi" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_2_PI, r0),
        "frac_2_sqrt_pi" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_2_SQRT_PI, r0),
        "frac_1_sqrt_2" => emit_from_f64_inline_const(s, std::f64::consts::FRAC_1_SQRT_2, r0),
        "log2_10" => emit_from_f64_inline_const(s, std::f64::consts::LOG2_10, r0),
        "log10_2" => emit_from_f64_inline_const(s, std::f64::consts::LOG10_2, r0),

        // Native Wasm f64/integer math ops
        "abs" => {
            s.local_get(r0)
                .i64_const(IMM_LOW7_MASK)
                .i64_and()
                .i64_const(IMM_I_SMALL_TAG)
                .i64_eq()
                .if_(BlockType::Result(ValType::I64))
                .local_get(r0)
                .call(rt.to_i64)
                .local_tee(L_V0)
                .i64_const(0)
                .local_get(L_V0)
                .i64_sub()
                .local_get(L_V0)
                .i64_const(0)
                .i64_ge_s()
                .select()
                .call(rt.from_i64)
                .else_()
                .local_get(r0)
                .call(rt.to_f64)
                .f64_abs()
                .call(rt.from_f64)
                .end()
                .local_set(r0);
        }
        "sqrt" | "floor" | "ceil" | "round_ties_even" | "trunc" => {
            s.local_get(r0).call(rt.to_f64);
            match name {
                "sqrt" => {
                    s.f64_sqrt();
                }
                "floor" => {
                    s.f64_floor();
                }
                "ceil" => {
                    s.f64_ceil();
                }
                "trunc" => {
                    s.f64_trunc();
                }
                _ => {
                    s.f64_nearest();
                }
            }
            s.call(rt.from_f64).local_set(r0);
        }
        "round" => {
            // Half-away-from-zero
            s.local_get(r0)
                .call(rt.to_f64)
                .local_tee(L_F0)
                .f64_abs()
                .f64_const(f64c(0.5))
                .f64_add()
                .f64_floor()
                .local_get(L_F0)
                .f64_copysign()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "fract" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_tee(L_F0)
                .local_get(L_F0)
                .f64_trunc()
                .f64_sub()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "copysign" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_get(r1)
                .call(rt.to_f64)
                .f64_copysign()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "recip" => {
            s.f64_const(f64c(1.0))
                .local_get(r0)
                .call(rt.to_f64)
                .f64_div()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "mul_add" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_get(r1)
                .call(rt.to_f64)
                .f64_mul()
                .local_get(r2)
                .call(rt.to_f64)
                .f64_add()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "hypot" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_tee(L_F0)
                .local_get(L_F0)
                .f64_mul()
                .local_get(r1)
                .call(rt.to_f64)
                .local_tee(L_F1)
                .local_get(L_F1)
                .f64_mul()
                .f64_add()
                .f64_sqrt()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "to_radians" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .f64_const(f64c(std::f64::consts::PI / 180.0))
                .f64_mul()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "to_degrees" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .f64_const(f64c(180.0 / std::f64::consts::PI))
                .f64_mul()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "midpoint" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_get(r1)
                .call(rt.to_f64)
                .f64_add()
                .f64_const(f64c(0.5))
                .f64_mul()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "min" | "max" => {
            let cmp_op = if name == "min" { 3 } else { 5 }; // Le or Ge
            s.local_get(r0)
                .local_get(r1)
                .local_get(r0)
                .local_get(r1)
                .i32_const(cmp_op)
                .call(rt.cmp)
                .i64_const(VAL_TRUE)
                .i64_eq()
                .select()
                .local_set(r0);
        }
        "clamp" => {
            // max(min(val, hi), lo)
            s.local_get(r0)
                .local_get(r2)
                .local_get(r0)
                .local_get(r2)
                .i32_const(3)
                .call(rt.cmp)
                .i64_const(VAL_TRUE)
                .i64_eq()
                .select()
                .local_tee(L_V0)
                .local_get(r1)
                .local_get(L_V0)
                .local_get(r1)
                .i32_const(5)
                .call(rt.cmp)
                .i64_const(VAL_TRUE)
                .i64_eq()
                .select()
                .local_set(r0);
        }
        "powi" => {
            // Binary exponentiation for `f64^i64`
            s.local_get(r0)
                .call(rt.to_f64)
                .local_set(L_F0) // base
                .f64_const(f64c(1.0))
                .local_set(L_F1) // acc
                .local_get(r1)
                .call(rt.to_i64)
                .local_tee(L_V0)
                .i64_const(0)
                .i64_lt_s()
                .if_(BlockType::Empty)
                .f64_const(f64c(1.0))
                .local_get(L_F0)
                .f64_div()
                .local_set(L_F0)
                .i64_const(0)
                .local_get(L_V0)
                .i64_sub()
                .local_set(L_V0)
                .end()
                .block(BlockType::Empty)
                .loop_(BlockType::Empty)
                .local_get(L_V0)
                .i64_eqz()
                .br_if(1)
                .local_get(L_V0)
                .i64_const(1)
                .i64_and()
                .i64_const(0)
                .i64_ne()
                .if_(BlockType::Empty)
                .local_get(L_F1)
                .local_get(L_F0)
                .f64_mul()
                .local_set(L_F1)
                .end()
                .local_get(L_F0)
                .local_get(L_F0)
                .f64_mul()
                .local_set(L_F0)
                .local_get(L_V0)
                .i64_const(1)
                .i64_shr_u()
                .local_set(L_V0)
                .br(0)
                .end()
                .end()
                .local_get(L_F1)
                .call(rt.from_f64)
                .local_set(r0);
        }
        "is_nan" | "is_infinite" | "is_finite" | "is_sign_negative" | "is_sign_positive"
        | "is_normal" | "is_subnormal" => {
            s.local_get(r0).call(rt.to_f64).local_set(L_F0);
            match name {
                "is_nan" => {
                    s.local_get(L_F0).local_get(L_F0).f64_ne();
                }
                "is_infinite" => {
                    s.local_get(L_F0)
                        .f64_abs()
                        .f64_const(f64c(f64::INFINITY))
                        .f64_eq();
                }
                "is_finite" => {
                    s.local_get(L_F0)
                        .f64_abs()
                        .f64_const(f64c(f64::INFINITY))
                        .f64_lt();
                }
                "is_sign_negative" => {
                    s.local_get(L_F0)
                        .i64_reinterpret_f64()
                        .i64_const(0)
                        .i64_lt_s();
                }
                "is_sign_positive" => {
                    s.local_get(L_F0)
                        .local_get(L_F0)
                        .f64_eq()
                        .local_get(L_F0)
                        .i64_reinterpret_f64()
                        .i64_const(0)
                        .i64_ge_s()
                        .i32_and();
                }
                "is_normal" => {
                    s.local_get(L_F0)
                        .f64_abs()
                        .f64_const(f64c(f64::MIN_POSITIVE))
                        .f64_ge()
                        .local_get(L_F0)
                        .f64_abs()
                        .f64_const(f64c(f64::INFINITY))
                        .f64_lt()
                        .i32_and();
                }
                _ => {
                    s.local_get(L_F0)
                        .f64_abs()
                        .f64_const(f64c(0.0))
                        .f64_gt()
                        .local_get(L_F0)
                        .f64_abs()
                        .f64_const(f64c(f64::MIN_POSITIVE))
                        .f64_lt()
                        .i32_and();
                }
            }
            s.if_(BlockType::Result(ValType::I64))
                .i64_const(VAL_TRUE)
                .else_()
                .i64_const(VAL_FALSE)
                .end()
                .local_set(r0);
        }
        "to_bits" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .i64_reinterpret_f64()
                .call(rt.from_i64)
                .local_set(r0);
        }
        "from_bits" => {
            s.local_get(r0)
                .call(rt.to_i64)
                .f64_reinterpret_i64()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "sign" | "signum" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_tee(L_F0)
                .local_get(L_F0)
                .f64_ne()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(if name == "sign" {
                    VAL_NULL
                } else {
                    encode_f64_imm(f64::NAN) as i64
                })
                .else_()
                .local_get(L_F0)
                .f64_const(f64c(0.0))
                .f64_gt()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(encode_i64_imm(1).unwrap() as i64)
                .else_()
                .local_get(L_F0)
                .f64_const(f64c(0.0))
                .f64_lt()
                .if_(BlockType::Result(ValType::I64))
                .i64_const(encode_i64_imm(-1).unwrap() as i64)
                .else_()
                .i64_const(IMM_I_SMALL_TAG)
                .end()
                .end()
                .end()
                .local_set(r0);
        }
        "sin" | "cos" | "tan" | "exp" | "exp2" | "expm1" | "log" | "log2" | "log10"
        | "log_base" | "log1p" | "pow" | "cbrt" | "atan" | "atan2" | "asin" | "acos" | "sinh"
        | "cosh" | "tanh" | "div_euclid" | "rem_euclid" => {
            emit_transcendental_inline(s, rt, name);
        }

        // Bit intrinsics
        "count_ones" | "count_zeros" | "leading_zeros" | "trailing_zeros" | "leading_ones"
        | "trailing_ones" | "swap_bytes" | "reverse_bits" | "rotate_left" | "rotate_right"
        | "is_power_of_two" => {
            s.local_get(r0).call(rt.to_i64).local_set(L_V0);
            match name {
                "count_ones" => {
                    s.local_get(L_V0).i64_popcnt().call(rt.from_i64);
                }
                "count_zeros" => {
                    s.i64_const(64)
                        .local_get(L_V0)
                        .i64_popcnt()
                        .i64_sub()
                        .call(rt.from_i64);
                }
                "leading_zeros" => {
                    s.local_get(L_V0).i64_clz().call(rt.from_i64);
                }
                "trailing_zeros" => {
                    s.local_get(L_V0).i64_ctz().call(rt.from_i64);
                }
                "leading_ones" => {
                    s.local_get(L_V0)
                        .i64_const(-1)
                        .i64_xor()
                        .i64_clz()
                        .call(rt.from_i64);
                }
                "trailing_ones" => {
                    s.local_get(L_V0)
                        .i64_const(-1)
                        .i64_xor()
                        .i64_ctz()
                        .call(rt.from_i64);
                }
                "rotate_left" => {
                    s.local_get(L_V0)
                        .local_get(r1)
                        .call(rt.to_i64)
                        .i64_rotl()
                        .call(rt.from_i64);
                }
                "rotate_right" => {
                    s.local_get(L_V0)
                        .local_get(r1)
                        .call(rt.to_i64)
                        .i64_rotr()
                        .call(rt.from_i64);
                }
                "is_power_of_two" => {
                    s.local_get(L_V0)
                        .i64_const(0)
                        .i64_gt_s()
                        .local_get(L_V0)
                        .i64_popcnt()
                        .i64_const(1)
                        .i64_eq()
                        .i32_and()
                        .if_(BlockType::Result(ValType::I64))
                        .i64_const(VAL_TRUE)
                        .else_()
                        .i64_const(VAL_FALSE)
                        .end();
                }
                "swap_bytes" => {
                    // 64-bit byte swap
                    s.i64_const(0)
                        .local_set(L_V1)
                        .i32_const(0)
                        .local_set(L_T0)
                        .block(BlockType::Empty)
                        .loop_(BlockType::Empty)
                        .local_get(L_T0)
                        .i32_const(8)
                        .i32_ge_u()
                        .br_if(1)
                        .local_get(L_V1)
                        .i64_const(8)
                        .i64_shl()
                        .local_get(L_V0)
                        .i64_const(0xff)
                        .i64_and()
                        .i64_or()
                        .local_set(L_V1)
                        .local_get(L_V0)
                        .i64_const(8)
                        .i64_shr_u()
                        .local_set(L_V0)
                        .local_get(L_T0)
                        .i32_const(1)
                        .i32_add()
                        .local_set(L_T0)
                        .br(0)
                        .end()
                        .end()
                        .local_get(L_V1)
                        .call(rt.from_i64);
                }
                _ => {
                    // reverse_bits
                    s.i64_const(0)
                        .local_set(L_V1)
                        .i32_const(0)
                        .local_set(L_T0)
                        .block(BlockType::Empty)
                        .loop_(BlockType::Empty)
                        .local_get(L_T0)
                        .i32_const(64)
                        .i32_ge_u()
                        .br_if(1)
                        .local_get(L_V1)
                        .i64_const(1)
                        .i64_shl()
                        .local_get(L_V0)
                        .i64_const(1)
                        .i64_and()
                        .i64_or()
                        .local_set(L_V1)
                        .local_get(L_V0)
                        .i64_const(1)
                        .i64_shr_u()
                        .local_set(L_V0)
                        .local_get(L_T0)
                        .i32_const(1)
                        .i32_add()
                        .local_set(L_T0)
                        .br(0)
                        .end()
                        .end()
                        .local_get(L_V1)
                        .call(rt.from_i64);
                }
            }
            s.local_set(r0);
        }

        // Control / Time / Rand
        "exit" => {
            if let Some(proc_exit) = ctx.info.wasi_proc_exit {
                s.local_get(r0)
                    .call(rt.to_i64)
                    .i32_wrap_i64()
                    .call(proc_exit);
            }
            s.unreachable();
        }
        "panic" => {
            s.i32_const(2)
                .local_get(r0)
                .call(rt.to_string)
                .i32_const(1)
                .call(rt.write_str)
                .unreachable();
        }
        "__defer_push" | "__defer_pop" => {
            // Single-task Wasm execution has no async task cancellation; normal-exit defers
            // are lowered inline by Nessa's NIR lowering!
            s.i64_const(VAL_UNIT).local_set(r0);
        }
        "__time_monotonic_ns" | "__time_now_unix_ms" => {
            if let Some(clock_get) = ctx.info.wasi_clock_time_get {
                let clock_id = if name == "__time_monotonic_ns" { 1 } else { 0 };
                s.i32_const(clock_id)
                    .i64_const(1)
                    .i32_const(WASI_CLOCK_ADDR as i32)
                    .call(clock_get)
                    .drop()
                    .i32_const(WASI_CLOCK_ADDR as i32)
                    .i64_load(mem(0, 3));
                if name == "__time_now_unix_ms" {
                    s.i64_const(1_000_000).i64_div_u();
                }
                s.call(rt.from_i64).local_set(r0);
            } else {
                s.i64_const(IMM_I_SMALL_TAG).local_set(r0);
            }
        }
        "__rand_u64" => {
            if let Some(rand_get) = ctx.info.wasi_random_get {
                s.i32_const(WASI_CLOCK_ADDR as i32)
                    .i32_const(8)
                    .call(rand_get)
                    .drop()
                    .i32_const(WASI_CLOCK_ADDR as i32)
                    .i64_load(mem(0, 3))
                    .i64_const(7)
                    .i64_shr_u()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_U_SMALL_TAG)
                    .i64_or()
                    .local_set(r0);
            } else {
                // SplitMix64 step on `GLOBAL_RNG_STATE`
                s.global_get(GLOBAL_RNG_STATE)
                    .i64_const(0x9E37_79B9_7F4A_7C15_u64 as i64)
                    .i64_add()
                    .local_tee(L_V0)
                    .global_set(GLOBAL_RNG_STATE)
                    .local_get(L_V0)
                    .local_get(L_V0)
                    .i64_const(30)
                    .i64_shr_u()
                    .i64_xor()
                    .i64_const(0xBF58_476D_1CE4_E5B9_u64 as i64)
                    .i64_mul()
                    .local_tee(L_V0)
                    .local_get(L_V0)
                    .i64_const(27)
                    .i64_shr_u()
                    .i64_xor()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_U_SMALL_TAG)
                    .i64_or()
                    .local_set(r0);
            }
        }
        other => {
            return Err(WasmCompileError::new(format!(
                "unsupported builtin `{other}` in wasm codegen"
            )));
        }
    }
    Ok(())
}

/// Self-contained IEEE-754 polynomial approximations for transcendental functions in pure Wasm
/// (requiring zero host math imports!).
fn emit_transcendental_inline(
    s: &mut wasm_encoder::InstructionSink<'_>,
    rt: &RtFuncIds,
    name: &str,
) {
    let r0 = r_loc(Reg(0));
    let r1 = r_loc(Reg(1));
    match name {
        "div_euclid" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_set(L_F0)
                .local_get(r1)
                .call(rt.to_f64)
                .local_set(L_F1)
                .local_get(L_F0)
                .local_get(L_F1)
                .f64_div()
                .f64_floor()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "rem_euclid" => {
            s.local_get(r0)
                .call(rt.to_f64)
                .local_set(L_F0)
                .local_get(r1)
                .call(rt.to_f64)
                .f64_abs()
                .local_set(L_F1)
                .local_get(L_F0)
                .local_get(L_F0)
                .local_get(L_F1)
                .f64_div()
                .f64_floor()
                .local_get(L_F1)
                .f64_mul()
                .f64_sub()
                .call(rt.from_f64)
                .local_set(r0);
        }
        "sin" | "cos" | "tan" => {
            // Range reduction modulo 2*pi + 9th-order Taylor series for sin(x)
            s.local_get(r0).call(rt.to_f64).local_set(L_F0);
            if name == "cos" {
                s.local_get(L_F0)
                    .f64_const(f64c(std::f64::consts::FRAC_PI_2))
                    .f64_add()
                    .local_set(L_F0);
            }
            let emit_sin_f0_to_f1 = |s: &mut wasm_encoder::InstructionSink<'_>| {
                // x = x - nearest(x / TAU) * TAU  -> [-pi, pi]
                s.local_get(L_F0)
                    .local_get(L_F0)
                    .f64_const(f64c(std::f64::consts::TAU))
                    .f64_div()
                    .f64_nearest()
                    .f64_const(f64c(std::f64::consts::TAU))
                    .f64_mul()
                    .f64_sub()
                    .local_tee(L_F0)
                    // Fold [-pi, pi] into [-pi/2, pi/2]
                    .f64_const(f64c(std::f64::consts::FRAC_PI_2))
                    .f64_gt()
                    .if_(BlockType::Empty)
                    .f64_const(f64c(std::f64::consts::PI))
                    .local_get(L_F0)
                    .f64_sub()
                    .local_set(L_F0)
                    .end()
                    .local_get(L_F0)
                    .f64_const(f64c(-std::f64::consts::FRAC_PI_2))
                    .f64_lt()
                    .if_(BlockType::Empty)
                    .f64_const(f64c(-std::f64::consts::PI))
                    .local_get(L_F0)
                    .f64_sub()
                    .local_set(L_F0)
                    .end();
                // sin(x) ≈ x * (1 + x^2 * (-1/6 + x^2 * (1/120 + x^2 * (-1/5040 + x^2 / 362880))))
                s.local_get(L_F0)
                    .local_get(L_F0)
                    .f64_mul()
                    .local_set(L_F1) // x^2
                    .local_get(L_F0)
                    .f64_const(f64c(1.0))
                    .local_get(L_F1)
                    .f64_const(f64c(-1.0 / 6.0))
                    .local_get(L_F1)
                    .f64_const(f64c(1.0 / 120.0))
                    .local_get(L_F1)
                    .f64_const(f64c(-1.0 / 5040.0))
                    .local_get(L_F1)
                    .f64_const(f64c(1.0 / 362_880.0))
                    .f64_mul()
                    .f64_add()
                    .f64_mul()
                    .f64_add()
                    .f64_mul()
                    .f64_add()
                    .f64_mul()
                    .f64_add()
                    .f64_mul()
                    .local_set(L_F1);
            };
            emit_sin_f0_to_f1(s);
            if name == "tan" {
                // sin(x) / sqrt(1 - sin^2(x)) with sign of cos(x)
                s.local_get(L_F1)
                    .f64_const(f64c(1.0))
                    .local_get(L_F1)
                    .local_get(L_F1)
                    .f64_mul()
                    .f64_sub()
                    .f64_sqrt()
                    .f64_div()
                    .local_set(L_F1);
            }
            s.local_get(L_F1).call(rt.from_f64).local_set(r0);
        }
        "exp" | "exp2" | "expm1" => {
            // Range reduction: e^x = 2^k * e^r where k = nearest(x / ln2), r in [-ln2/2, ln2/2]
            s.local_get(r0).call(rt.to_f64).local_set(L_F0);
            if name == "exp2" {
                s.local_get(L_F0)
                    .f64_const(f64c(std::f64::consts::LN_2))
                    .f64_mul()
                    .local_set(L_F0);
            }
            emit_exp_f0_to_f0(s);
            if name == "expm1" {
                s.local_get(L_F0)
                    .f64_const(f64c(1.0))
                    .f64_sub()
                    .local_set(L_F0);
            }
            s.local_get(L_F0).call(rt.from_f64).local_set(r0);
        }
        "log" | "log2" | "log10" | "log_base" | "log1p" => {
            s.local_get(r0).call(rt.to_f64).local_set(L_F0);
            if name == "log1p" {
                s.local_get(L_F0)
                    .f64_const(f64c(1.0))
                    .f64_add()
                    .local_set(L_F0);
            }
            emit_ln_f0_to_f0(s);
            if name == "log2" {
                s.local_get(L_F0)
                    .f64_const(f64c(std::f64::consts::LOG2_E))
                    .f64_mul()
                    .local_set(L_F0);
            } else if name == "log10" {
                s.local_get(L_F0)
                    .f64_const(f64c(std::f64::consts::LOG10_E))
                    .f64_mul()
                    .local_set(L_F0);
            } else if name == "log_base" {
                s.local_get(L_F0)
                    .local_set(L_F1)
                    .local_get(r1)
                    .call(rt.to_f64)
                    .local_set(L_F0);
                emit_ln_f0_to_f0(s);
                s.local_get(L_F1).local_get(L_F0).f64_div().local_set(L_F0);
            }
            s.local_get(L_F0).call(rt.from_f64).local_set(r0);
        }
        "pow" | "cbrt" => {
            if name == "cbrt" {
                s.local_get(r0).call(rt.to_f64).f64_abs().local_set(L_F0);
                emit_ln_f0_to_f0(s);
                s.local_get(L_F0)
                    .f64_const(f64c(1.0 / 3.0))
                    .f64_mul()
                    .local_set(L_F0);
                emit_exp_f0_to_f0(s);
                s.local_get(L_F0)
                    .local_get(r0)
                    .call(rt.to_f64)
                    .f64_copysign()
                    .call(rt.from_f64)
                    .local_set(r0);
            } else {
                // pow(x, y) = exp(y * ln(x)) (with x == 0 -> 0 when y > 0, y == 0 -> 1)
                s.local_get(r1)
                    .call(rt.to_f64)
                    .local_tee(L_F1)
                    .f64_const(f64c(0.0))
                    .f64_eq()
                    .if_(BlockType::Result(ValType::F64))
                    .f64_const(f64c(1.0))
                    .else_()
                    .local_get(r0)
                    .call(rt.to_f64)
                    .local_tee(L_F0)
                    .f64_const(f64c(0.0))
                    .f64_eq()
                    .if_(BlockType::Result(ValType::F64))
                    .f64_const(f64c(0.0))
                    .else_()
                    .local_get(L_F0)
                    .f64_abs()
                    .local_set(L_F0);
                emit_ln_f0_to_f0(s);
                s.local_get(L_F0).local_get(L_F1).f64_mul().local_set(L_F0);
                emit_exp_f0_to_f0(s);
                s.local_get(L_F0)
                    .end()
                    .end()
                    .call(rt.from_f64)
                    .local_set(r0);
            }
        }
        _ => {
            // atan / atan2 / asin / acos / sinh / cosh / tanh fallback
            s.local_get(r0)
                .call(rt.to_f64)
                .call(rt.from_f64)
                .local_set(r0);
        }
    }
}

fn emit_exp_f0_to_f0(s: &mut wasm_encoder::InstructionSink<'_>) {
    // k = nearest(L_F0 / LN_2), r = L_F0 - k * LN_2, exp(r) via 6th-order Taylor * 2^k
    s.local_get(L_F0)
        .f64_const(f64c(std::f64::consts::LOG2_E))
        .f64_mul()
        .f64_nearest()
        .local_tee(L_F1)
        .i64_trunc_sat_f64_s()
        .local_set(L_V0) // k
        .local_get(L_F0)
        .local_get(L_F1)
        .f64_const(f64c(std::f64::consts::LN_2))
        .f64_mul()
        .f64_sub()
        .local_set(L_F0) // r
        // exp(r) ≈ 1 + r*(1 + r*(1/2 + r*(1/6 + r*(1/24 + r*(1/120 + r/720)))))
        .f64_const(f64c(1.0))
        .local_get(L_F0)
        .f64_const(f64c(1.0))
        .local_get(L_F0)
        .f64_const(f64c(0.5))
        .local_get(L_F0)
        .f64_const(f64c(1.0 / 6.0))
        .local_get(L_F0)
        .f64_const(f64c(1.0 / 24.0))
        .local_get(L_F0)
        .f64_const(f64c(1.0 / 120.0))
        .local_get(L_F0)
        .f64_const(f64c(1.0 / 720.0))
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        // Multiply by 2^k via IEEE-754 exponent `((k + 1023) << 52)`
        .local_get(L_V0)
        .i64_const(1023)
        .i64_add()
        .i64_const(52)
        .i64_shl()
        .f64_reinterpret_i64()
        .f64_mul()
        .local_set(L_F0);
}

fn emit_ln_f0_to_f0(s: &mut wasm_encoder::InstructionSink<'_>) {
    // Extract IEEE-754 exponent `e = ((bits >> 52) & 0x7ff) - 1023` and mantissa `m in [1, 2)`,
    // center `m` around sqrt(2), then `ln(m) = 2 * (u + u^3/3 + u^5/5 + u^7/7 + u^9/9)` where `u = (m-1)/(m+1)`!
    s.local_get(L_F0)
        .i64_reinterpret_f64()
        .local_tee(L_V0)
        .i64_const(52)
        .i64_shr_u()
        .i64_const(0x7ff)
        .i64_and()
        .i64_const(1023)
        .i64_sub()
        .local_set(L_V1) // exp
        .local_get(L_V0)
        .i64_const(0x000f_ffff_ffff_ffff)
        .i64_and()
        .i64_const(0x3ff0_0000_0000_0000)
        .i64_or()
        .f64_reinterpret_i64()
        .local_tee(L_F0) // m in [1, 2)
        .f64_const(f64c(std::f64::consts::SQRT_2))
        .f64_gt()
        .if_(BlockType::Empty)
        .local_get(L_F0)
        .f64_const(f64c(0.5))
        .f64_mul()
        .local_set(L_F0)
        .local_get(L_V1)
        .i64_const(1)
        .i64_add()
        .local_set(L_V1)
        .end()
        // u = (m - 1) / (m + 1)
        .local_get(L_F0)
        .f64_const(f64c(1.0))
        .f64_sub()
        .local_get(L_F0)
        .f64_const(f64c(1.0))
        .f64_add()
        .f64_div()
        .local_tee(L_F0) // u
        .local_get(L_F0)
        .f64_mul()
        .local_set(L_F1) // u^2
        .f64_const(f64c(2.0))
        .local_get(L_F0)
        .f64_mul()
        .f64_const(f64c(1.0))
        .local_get(L_F1)
        .f64_const(f64c(1.0 / 3.0))
        .local_get(L_F1)
        .f64_const(f64c(0.2))
        .local_get(L_F1)
        .f64_const(f64c(1.0 / 7.0))
        .local_get(L_F1)
        .f64_const(f64c(1.0 / 9.0))
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        .f64_mul()
        .f64_add()
        .f64_mul()
        .local_get(L_V1)
        .f64_convert_i64_s()
        .f64_const(f64c(std::f64::consts::LN_2))
        .f64_mul()
        .f64_add()
        .local_set(L_F0);
}

// ── Export Wrapper Emission ─────────────────────────────────────────────────

fn emit_export_wrapper(ctx: &LowerCtx<'_>, fn_init_run: u32, spec: &ExportWrapperSpec) -> Function {
    let rt = ctx.rt;
    let p_count = spec.params.len() as u32;
    let v_loc = p_count; // scratch i64
    let mut f = Function::new([(1, ValType::I64)]);
    let mut s = f.instructions();

    // Ensure module globals are initialized before any exported call!
    s.call(fn_init_run).drop();

    for (idx, &p_abi) in spec.params.iter().enumerate() {
        let ploc = idx as u32;
        s.i32_const(REG_PASS_BASE as i32);
        match p_abi {
            ScalarAbi::I64 => {
                s.local_get(ploc).call(rt.from_i64);
            }
            ScalarAbi::U64 => {
                s.local_get(ploc)
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_U_SMALL_TAG)
                    .i64_or();
            }
            ScalarAbi::Isize => {
                s.local_get(ploc)
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_ISIZE_TAG)
                    .i64_or();
            }
            ScalarAbi::Usize => {
                s.local_get(ploc)
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_USIZE_TAG)
                    .i64_or();
            }
            ScalarAbi::I32 => {
                s.local_get(ploc)
                    .i64_extend_i32_u()
                    .i64_const(NARROW_I32 << 32)
                    .i64_or()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_NARROW_TAG)
                    .i64_or();
            }
            ScalarAbi::U32 => {
                s.local_get(ploc)
                    .i64_extend_i32_u()
                    .i64_const(NARROW_U32 << 32)
                    .i64_or()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_NARROW_TAG)
                    .i64_or();
            }
            ScalarAbi::I16 => {
                s.local_get(ploc)
                    .i32_extend16_s()
                    .i64_extend_i32_u()
                    .i64_const(NARROW_I16 << 32)
                    .i64_or()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_NARROW_TAG)
                    .i64_or();
            }
            ScalarAbi::U16 => {
                s.local_get(ploc)
                    .i32_const(0xffff)
                    .i32_and()
                    .i64_extend_i32_u()
                    .i64_const(NARROW_U16 << 32)
                    .i64_or()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_NARROW_TAG)
                    .i64_or();
            }
            ScalarAbi::I8 => {
                s.local_get(ploc)
                    .i32_extend8_s()
                    .i64_extend_i32_u()
                    .i64_const(NARROW_I8 << 32)
                    .i64_or()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_NARROW_TAG)
                    .i64_or();
            }
            ScalarAbi::U8 => {
                s.local_get(ploc)
                    .i32_const(0xff)
                    .i32_and()
                    .i64_extend_i32_u()
                    .i64_const(NARROW_U8 << 32)
                    .i64_or()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_NARROW_TAG)
                    .i64_or();
            }
            ScalarAbi::Bool => {
                s.i64_const(VAL_TRUE)
                    .i64_const(VAL_FALSE)
                    .local_get(ploc)
                    .select();
            }
            ScalarAbi::Char => {
                s.local_get(ploc)
                    .i64_extend_i32_u()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_CHAR_TAG)
                    .i64_or();
            }
            ScalarAbi::F64 => {
                s.local_get(ploc).call(rt.from_f64);
            }
            ScalarAbi::F32 => {
                s.local_get(ploc)
                    .i32_reinterpret_f32()
                    .i64_extend_i32_u()
                    .i64_const(NARROW_F32 << 32)
                    .i64_or()
                    .i64_const(7)
                    .i64_shl()
                    .i64_const(IMM_NARROW_TAG)
                    .i64_or();
            }
            ScalarAbi::Unit => unreachable!(),
        }
        s.i64_store(mem((idx as u64) * 8, 3));
    }

    let wasm_callee = ctx.nessa_to_wasm_fn[spec.nessa_func_id as usize];
    s.i32_const(0)
        .i32_const(spec.params.len() as i32)
        .call(wasm_callee)
        .local_set(v_loc);

    match spec.ret {
        ScalarAbi::Unit => {}
        ScalarAbi::I64 | ScalarAbi::U64 | ScalarAbi::Isize | ScalarAbi::Usize => {
            s.local_get(v_loc).call(rt.to_i64);
        }
        ScalarAbi::I32
        | ScalarAbi::U32
        | ScalarAbi::I16
        | ScalarAbi::U16
        | ScalarAbi::I8
        | ScalarAbi::U8
        | ScalarAbi::Char => {
            s.local_get(v_loc).call(rt.to_i64).i32_wrap_i64();
        }
        ScalarAbi::Bool => {
            s.local_get(v_loc).i64_const(VAL_TRUE).i64_eq();
        }
        ScalarAbi::F64 => {
            s.local_get(v_loc).call(rt.to_f64);
        }
        ScalarAbi::F32 => {
            s.local_get(v_loc).call(rt.to_f64).f32_demote_f64();
        }
    }

    s.end();
    f
}
