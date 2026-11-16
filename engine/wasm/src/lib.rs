//! WebAssembly subsystem for Nessa (`engine/wasm`).
//!
//! Provides two complementary capabilities:
//! 1. **Embedded WebAssembly Runtime (`module`, `exec`, `wasi`)**: Validates and executes
//!    standard W3C WebAssembly 2.0 (`+ simd128`, `bulk-memory`, `multi-value`, `tail-call`,
//!    `reference-types`, `sign-extension`, `saturating-float-to-int`) modules inside a
//!    memory-, fuel-, and stack-bounded sandbox, backing `Opcode::CallWasm`, NSBC Archive
//!    `SectionKind::Wasm`, `std.wasm`, and `nessa run <file.wasm>`.
//! 2. **Nessa-to-WebAssembly AOT Compiler (`aot`)**: Compiles validated Nessa
//!    [`nsbc::CompiledArtifact`] units to standalone `.wasm` binaries targeting either
//!    `wasm32-wasi` (`wasi_snapshot_preview1`) or `wasm32-freestanding`.

pub mod aot;
mod aot_runtime;
mod exec;
mod module;
mod simd;
pub mod wasi;

use std::fmt;

pub use aot::{WasmCompileError, WasmCompileOptions, WasmTarget, compile_artifact_to_wasm};
pub use exec::{HostDispatcher, HostFuncMap, WasmCaller, WasmInstance};
pub use module::{FuncSig, WasmModule};
pub use wasi::{WasiConfig, WasiOutput, run_wasi_bytes, run_wasi_module};

/// Value types supported across the Nessa <-> WebAssembly boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WasmValType {
    I32,
    I64,
    F32,
    F64,
    V128,
    FuncRef,
    ExternRef,
}

impl WasmValType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::V128 => "v128",
            Self::FuncRef => "funcref",
            Self::ExternRef => "externref",
        }
    }
}

impl fmt::Display for WasmValType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Runtime WebAssembly value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmVal {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    V128(u128),
    /// `None` represents `ref.null func`; `Some(idx)` names a module function index.
    FuncRef(Option<u32>),
    /// `None` represents `ref.null extern`; `Some(id)` names a host extern token.
    ExternRef(Option<u64>),
}

impl WasmVal {
    #[inline]
    pub fn f32(v: f32) -> Self {
        Self::F32(v.to_bits())
    }

    #[inline]
    pub fn f64(v: f64) -> Self {
        Self::F64(v.to_bits())
    }

    #[inline]
    pub fn as_f32(self) -> Option<f32> {
        match self {
            Self::F32(bits) => Some(f32::from_bits(bits)),
            _ => None,
        }
    }

    #[inline]
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::F64(bits) => Some(f64::from_bits(bits)),
            _ => None,
        }
    }

    #[inline]
    pub const fn val_type(&self) -> WasmValType {
        match self {
            Self::I32(_) => WasmValType::I32,
            Self::I64(_) => WasmValType::I64,
            Self::F32(_) => WasmValType::F32,
            Self::F64(_) => WasmValType::F64,
            Self::V128(_) => WasmValType::V128,
            Self::FuncRef(_) => WasmValType::FuncRef,
            Self::ExternRef(_) => WasmValType::ExternRef,
        }
    }

    #[inline]
    pub const fn default_for(ty: WasmValType) -> Self {
        match ty {
            WasmValType::I32 => Self::I32(0),
            WasmValType::I64 => Self::I64(0),
            WasmValType::F32 => Self::F32(0),
            WasmValType::F64 => Self::F64(0),
            WasmValType::V128 => Self::V128(0),
            WasmValType::FuncRef => Self::FuncRef(None),
            WasmValType::ExternRef => Self::ExternRef(None),
        }
    }
}

/// Deterministic WebAssembly execution traps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmTrap {
    Unreachable,
    IntegerDivisionByZero,
    IntegerOverflow,
    InvalidConversionToInteger,
    OutOfBoundsMemoryAccess,
    OutOfBoundsTableAccess,
    IndirectCallNull,
    IndirectCallSignatureMismatch,
    CallStackExhausted,
    OutOfFuel,
    HostTrap(String),
    Exit(i32),
}

impl fmt::Display for WasmTrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable => f.write_str("wasm trap: unreachable executed"),
            Self::IntegerDivisionByZero => f.write_str("wasm trap: integer divide by zero"),
            Self::IntegerOverflow => f.write_str("wasm trap: integer overflow"),
            Self::InvalidConversionToInteger => {
                f.write_str("wasm trap: invalid conversion to integer")
            }
            Self::OutOfBoundsMemoryAccess => f.write_str("wasm trap: out of bounds memory access"),
            Self::OutOfBoundsTableAccess => f.write_str("wasm trap: out of bounds table access"),
            Self::IndirectCallNull => f.write_str("wasm trap: uninitialized/null table element"),
            Self::IndirectCallSignatureMismatch => {
                f.write_str("wasm trap: indirect call type mismatch")
            }
            Self::CallStackExhausted => f.write_str("wasm trap: call stack exhausted"),
            Self::OutOfFuel => f.write_str("wasm trap: execution fuel exhausted"),
            Self::HostTrap(msg) => write!(f, "wasm host trap: {msg}"),
            Self::Exit(code) => write!(f, "wasi proc_exit({code})"),
        }
    }
}

impl std::error::Error for WasmTrap {}

/// Errors produced when validating, instantiating, or calling WebAssembly modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmError {
    Validation(String),
    UnsupportedFeature(String),
    LinkError(String),
    UnknownExport(String),
    SignatureMismatch(String),
    MemoryLimitExceeded(String),
    ImmutableGlobal(String),
    Trap(WasmTrap),
}

impl fmt::Display for WasmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(msg) => write!(f, "invalid wasm module: {msg}"),
            Self::UnsupportedFeature(msg) => write!(f, "unsupported wasm construct: {msg}"),
            Self::LinkError(msg) => write!(f, "wasm link error: {msg}"),
            Self::UnknownExport(name) => write!(f, "unknown wasm export `{name}`"),
            Self::SignatureMismatch(msg) => write!(f, "wasm signature mismatch: {msg}"),
            Self::MemoryLimitExceeded(msg) => write!(f, "wasm memory limit exceeded: {msg}"),
            Self::ImmutableGlobal(name) => write!(f, "wasm global `{name}` is immutable"),
            Self::Trap(trap) => Display::fmt(trap, f),
        }
    }
}

use fmt::Display;

impl std::error::Error for WasmError {}

impl From<WasmTrap> for WasmError {
    fn from(trap: WasmTrap) -> Self {
        Self::Trap(trap)
    }
}

/// Resource limits for sandboxed WebAssembly instantiation and execution.
#[derive(Debug, Clone, Copy)]
pub struct WasmConfig {
    /// Maximum linear memory size in 64 KiB pages (default: `1024` = 64 MiB).
    pub max_memory_pages: u32,
    /// Maximum nested Wasm call frames (default: `1024`).
    pub max_call_depth: usize,
    /// Optional instruction fuel budget (`None` = unlimited).
    pub fuel_limit: Option<u64>,
}

impl Default for WasmConfig {
    fn default() -> Self {
        Self {
            max_memory_pages: 1024,
            max_call_depth: 1024,
            fuel_limit: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmExportKind {
    Func(FuncSig),
    Memory {
        min_pages: u32,
        max_pages: Option<u32>,
    },
    Table {
        element: WasmValType,
        min: u32,
        max: Option<u32>,
    },
    Global {
        val_type: WasmValType,
        mutable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmExportDesc {
    pub name: String,
    pub kind: WasmExportKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmImportKind {
    Func(FuncSig),
    Memory {
        min_pages: u32,
        max_pages: Option<u32>,
    },
    Table {
        element: WasmValType,
        min: u32,
        max: Option<u32>,
    },
    Global {
        val_type: WasmValType,
        mutable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmImportDesc {
    pub module: String,
    pub name: String,
    pub kind: WasmImportKind,
}

/// Validate raw `.wasm` binary bytes according to the W3C WebAssembly specification.
pub fn validate_wasm_bytes(bytes: &[u8]) -> Result<(), WasmError> {
    module::validate_only(bytes)
}

/// Format a human-readable structural summary / disassembly of a `.wasm` binary.
pub fn inspect_wasm_bytes(bytes: &[u8]) -> Result<String, WasmError> {
    let m = WasmModule::from_bytes(bytes)?;
    Ok(m.describe_summary())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use nsbc::{
        BuiltinImport, CodegenOutput, CompiledArtifact, CompiledFunction, Constant, FuncId,
        FunctionAbi, Instruction, Reg,
    };
    use type_pool::{Intrinsic, TypeKind, TypePool};
    use wasm_encoder::{
        CodeSection, ConstExpr, DataSection, ExportKind, ExportSection, Function, FunctionSection,
        GlobalSection, GlobalType, ImportSection, MemArg, MemorySection, MemoryType, Module,
        TypeSection, ValType,
    };

    use super::*;

    #[test]
    fn validates_and_executes_arithmetic_globals_memory_and_traps() {
        // Build a module with:
        // - memory 1 page
        // - mutable global counter (i32 = 10)
        // - `add_and_bump(a: i32, b: i32) -> i32` = (a + b) + (++counter)
        // - `div_s(a: i32, b: i32) -> i32`
        let mut module = Module::new();
        let mut types = TypeSection::new();
        types
            .ty()
            .function([ValType::I32, ValType::I32], [ValType::I32]);
        module.section(&types);

        let mut funcs = FunctionSection::new();
        funcs.function(0);
        funcs.function(0);
        module.section(&funcs);

        let mut mems = MemorySection::new();
        mems.memory(MemoryType {
            minimum: 1,
            maximum: Some(4),
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        module.section(&mems);

        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(10),
        );
        module.section(&globals);

        let mut exports = ExportSection::new();
        exports.export("memory", ExportKind::Memory, 0);
        exports.export("counter", ExportKind::Global, 0);
        exports.export("add_and_bump", ExportKind::Func, 0);
        exports.export("div_s", ExportKind::Func, 1);
        module.section(&exports);

        let mut code = CodeSection::new();
        let mut f0 = Function::new([]);
        f0.instructions()
            .global_get(0)
            .i32_const(1)
            .i32_add()
            .global_set(0)
            .local_get(0)
            .local_get(1)
            .i32_add()
            .global_get(0)
            .i32_add()
            .end();
        code.function(&f0);

        let mut f1 = Function::new([]);
        f1.instructions()
            .local_get(0)
            .local_get(1)
            .i32_div_s()
            .end();
        code.function(&f1);
        module.section(&code);

        let bytes = module.finish();
        validate_wasm_bytes(&bytes).unwrap();
        let summary = inspect_wasm_bytes(&bytes).unwrap();
        assert!(summary.contains("add_and_bump"));

        let compiled = Arc::new(WasmModule::from_bytes(&bytes).unwrap());
        let mut inst = WasmInstance::instantiate(
            Arc::clone(&compiled),
            WasmConfig {
                max_memory_pages: 4,
                max_call_depth: 64,
                fuel_limit: Some(500),
            },
            HostFuncMap::new(),
            &HashMap::new(),
        )
        .unwrap();

        let out = inst
            .call_export("add_and_bump", &[WasmVal::I32(20), WasmVal::I32(11)])
            .unwrap();
        assert_eq!(out, vec![WasmVal::I32(42)]);

        assert_eq!(inst.get_global("counter").unwrap(), WasmVal::I32(11));
        inst.set_global("counter", WasmVal::I32(99)).unwrap();
        assert_eq!(inst.get_global("counter").unwrap(), WasmVal::I32(99));

        // Linear memory roundtrip & OOB trap check
        inst.write_memory(128, b"nessa-wasm").unwrap();
        assert_eq!(inst.read_memory(128, 10).unwrap(), b"nessa-wasm");
        assert_eq!(
            inst.read_memory(65530, 16),
            Err(WasmError::Trap(WasmTrap::OutOfBoundsMemoryAccess))
        );

        // Division by zero & i32::MIN / -1 overflow traps
        assert_eq!(
            inst.call_export("div_s", &[WasmVal::I32(10), WasmVal::I32(0)]),
            Err(WasmError::Trap(WasmTrap::IntegerDivisionByZero))
        );
        assert_eq!(
            inst.call_export("div_s", &[WasmVal::I32(i32::MIN), WasmVal::I32(-1)]),
            Err(WasmError::Trap(WasmTrap::IntegerOverflow))
        );
    }

    #[test]
    fn executes_simd128_dot_product_tail_calls_host_callbacks_and_fuel_limits() {
        let mut module = Module::new();
        let mut types = TypeSection::new();
        // 0: host import (i32) -> i32
        types.ty().function([ValType::I32], [ValType::I32]);
        // 1: simd_sum4(ptr: i32) -> i32
        types.ty().function([ValType::I32], [ValType::I32]);
        // 2: tail_sum(n: i32, acc: i32) -> i32
        types
            .ty()
            .function([ValType::I32, ValType::I32], [ValType::I32]);
        module.section(&types);

        let mut imports = ImportSection::new();
        imports.import("env", "scale", wasm_encoder::EntityType::Function(0));
        module.section(&imports);

        let mut funcs = FunctionSection::new();
        funcs.function(1); // func 1: simd_sum4
        funcs.function(2); // func 2: tail_sum
        module.section(&funcs);

        let mut mems = MemorySection::new();
        mems.memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        module.section(&mems);

        let mut exports = ExportSection::new();
        exports.export("simd_sum4", ExportKind::Func, 1);
        exports.export("tail_sum", ExportKind::Func, 2);
        module.section(&exports);

        let mut code = CodeSection::new();
        // func 1: load v128 at ptr, add [1, 2, 3, 4], sum all 4 lanes, pass to `env.scale`
        let mut f_simd = Function::new([(1, ValType::V128)]);
        let cvec = u128::from_le_bytes({
            let mut b = [0u8; 16];
            b[0..4].copy_from_slice(&1i32.to_le_bytes());
            b[4..8].copy_from_slice(&2i32.to_le_bytes());
            b[8..12].copy_from_slice(&3i32.to_le_bytes());
            b[12..16].copy_from_slice(&4i32.to_le_bytes());
            b
        }) as i128;
        f_simd
            .instructions()
            .local_get(0)
            .v128_load(MemArg {
                offset: 0,
                align: 4,
                memory_index: 0,
            })
            .v128_const(cvec)
            .i32x4_add()
            .local_tee(1)
            .i32x4_extract_lane(0)
            .local_get(1)
            .i32x4_extract_lane(1)
            .i32_add()
            .local_get(1)
            .i32x4_extract_lane(2)
            .i32_add()
            .local_get(1)
            .i32x4_extract_lane(3)
            .i32_add()
            .call(0) // env.scale
            .end();
        code.function(&f_simd);

        // func 2: tail-recursive sum `tail_sum(n, acc)` using `return_call` (runs in O(1) call depth!)
        let mut f_tail = Function::new([]);
        f_tail
            .instructions()
            .local_get(0)
            .i32_eqz()
            .if_(wasm_encoder::BlockType::Empty)
            .local_get(1)
            .return_()
            .end()
            .local_get(0)
            .i32_const(1)
            .i32_sub()
            .local_get(1)
            .local_get(0)
            .i32_add()
            .return_call(2)
            .end();
        code.function(&f_tail);
        module.section(&code);

        // Pre-populate `[10, 20, 30, 40]` at offset 0
        let mut init_bytes = Vec::new();
        for x in [10i32, 20, 30, 40] {
            init_bytes.extend_from_slice(&x.to_le_bytes());
        }
        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(0), init_bytes);
        module.section(&data);

        let bytes = module.finish();
        let compiled = Arc::new(WasmModule::from_bytes(&bytes).unwrap());
        let mut host = HostFuncMap::new();
        host.insert(
            ("env".into(), "scale".into()),
            Box::new(|_caller: &mut WasmCaller<'_>, args: &[WasmVal]| {
                let WasmVal::I32(v) = args[0] else {
                    unreachable!()
                };
                Ok(vec![WasmVal::I32(v * 3)])
            }),
        );

        let mut inst = WasmInstance::instantiate(
            Arc::clone(&compiled),
            WasmConfig {
                max_memory_pages: 2,
                max_call_depth: 8, // tiny call depth proves `return_call` reuses frame!
                fuel_limit: Some(50_000),
            },
            host,
            &HashMap::new(),
        )
        .unwrap();

        // ((10+1) + (20+2) + (30+3) + (40+4)) * 3 = 110 * 3 = 330
        let res = inst.call_export("simd_sum4", &[WasmVal::I32(0)]).unwrap();
        assert!(matches!(res.as_slice(), [WasmVal::I32(330)]));

        // 1000-iteration tail call inside `max_call_depth: 8`!
        let sum = inst
            .call_export("tail_sum", &[WasmVal::I32(1000), WasmVal::I32(0)])
            .unwrap();
        assert!(matches!(sum.as_slice(), [WasmVal::I32(500_500)]));

        // Fuel exhaustion trap
        inst.set_fuel(Some(20));
        assert_eq!(
            inst.call_export("tail_sum", &[WasmVal::I32(100), WasmVal::I32(0)]),
            Err(WasmError::Trap(WasmTrap::OutOfFuel))
        );
    }

    #[test]
    fn aot_compiles_nessa_artifact_to_valid_wasi_and_freestanding_wasm() {
        let mut pool = TypePool::with_intrinsics();
        let i64_ty = Intrinsic::I64.type_index();
        let unit_ty = Intrinsic::Unit.type_index();
        let str_ty = Intrinsic::Str.type_index();
        let add_fn_ty = pool.intern_structural(TypeKind::Function {
            params: vec![i64_ty, i64_ty],
            ret: i64_ty,
        });
        let main_fn_ty = pool.intern_structural(TypeKind::Function {
            params: vec![],
            ret: unit_ty,
        });

        // FuncId(0): `compute(a: i64, b: i64) -> i64 { a * b + 2 }`
        let f_compute = CompiledFunction {
            display_owner: None,
            func_id: FuncId(0),
            name: str_interner::intern("compute"),
            instructions: vec![
                Instruction::r_type(nsbc::Opcode::Mul, Reg(2), Reg(0), Reg(1)).encode(),
                Instruction::load_imm(Reg(3), 2).encode(),
                Instruction::add(Reg(0), Reg(2), Reg(3)).encode(),
                Instruction::ret(Reg(0)).encode(),
            ],
            register_count: 8,
            param_count: 2,
            is_closure: false,
            function_type: add_fn_ty,
            abi: FunctionAbi::value_only(2),
            safepoint_pcs: vec![],
        };

        // FuncId(1): entry prints `"result="` + `compute(6, 7)` via `println`
        let f_entry = CompiledFunction {
            display_owner: None,
            func_id: FuncId(1),
            name: str_interner::intern("__entry"),
            instructions: vec![
                Instruction::load_const(Reg(4), 0).encode(), // "ok:"
                Instruction::load_imm(Reg(0), 6).encode(),
                Instruction::load_imm(Reg(1), 7).encode(),
                Instruction::call(0, 2).encode(),
                Instruction::call_builtin(5, 1).encode(), // to_string(44)
                Instruction::add(Reg(0), Reg(4), Reg(0)).encode(), // "ok:44"
                Instruction::call_builtin(1, 1).encode(), // println("ok:44")
                Instruction::return_unit().encode(),
            ],
            register_count: 8,
            param_count: 0,
            is_closure: false,
            function_type: main_fn_ty,
            abi: FunctionAbi::value_only(0),
            safepoint_pcs: vec![],
        };
        let _ = str_ty;

        let artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                functions: vec![f_compute, f_entry],
                constants: vec![Constant::Str("ok:".into())],
                globals: vec![],
                method_call_scopes: vec![],
            },
            type_pool: pool,
            entry: Some(FuncId(1)),
            builtin_abi_version: 12,
            builtins: vec![
                BuiltinImport {
                    id: 1,
                    name: "println".into(),
                },
                BuiltinImport {
                    id: 5,
                    name: "to_string".into(),
                },
            ],
            wasm_modules: vec![],
        };

        // 1. WASI target: run via `run_wasi_bytes` and verify captured stdout + exported `compute`!
        let wasi_bytes = compile_artifact_to_wasm(
            &artifact,
            &WasmCompileOptions {
                target: WasmTarget::WasiPreview1,
                export_all_pub: true,
            },
        )
        .unwrap();
        let wasi_out = run_wasi_bytes(&wasi_bytes, WasiConfig::default()).unwrap();
        assert_eq!(wasi_out.exit_code, 0);
        assert_eq!(String::from_utf8_lossy(&wasi_out.stdout), "ok:44\n");

        // 2. Freestanding target: zero imports, instantiate directly and call `compute(10, 5)`!
        let free_bytes = compile_artifact_to_wasm(
            &artifact,
            &WasmCompileOptions {
                target: WasmTarget::Freestanding,
                export_all_pub: true,
            },
        )
        .unwrap();
        let free_mod = Arc::new(WasmModule::from_bytes(&free_bytes).unwrap());
        assert!(free_mod.imports().is_empty());
        let mut free_inst = WasmInstance::instantiate(
            free_mod,
            WasmConfig::default(),
            HostFuncMap::new(),
            &HashMap::new(),
        )
        .unwrap();
        let res = free_inst
            .call_export("compute", &[WasmVal::I64(10), WasmVal::I64(5)])
            .unwrap();
        assert!(matches!(res.as_slice(), [WasmVal::I64(52)]));
    }
}
