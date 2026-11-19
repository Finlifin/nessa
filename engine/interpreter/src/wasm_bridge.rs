//! WebAssembly execution bridge for `Opcode::CallWasm` and `std.wasm` builtins.
//!
//! Coordinates two execution paths over `engine/wasm`:
//! 1. **Zero-boilerplate embedded bindings (`Opcode::CallWasm`)** backed by NSBC Archive
//!    `SectionKind::Wasm = 7`, marshaling Nessa scalars and `String`/`Bytes` slices across
//!    linear memory inside a `Vm::with_native_frame` guard.
//! 2. **Dynamic `std.wasm` host API (`WasmModule`, `WasmInstance`, `run_wasi`)** with
//!    GC-rooted Nessa host-import closures scanned in `scan_vm_roots` and synchronous
//!    callback execution guarded against cross-frame continuation capture.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nsbc::{Reg, WasmModuleDef};
use parking_lot::Mutex;
use runtime::{Number, TaggedValue, TaskId};
use type_pool::{CollectionRole, Intrinsic, TypeKind};
use wasm::{
    FuncSig, HostFuncMap, WasiConfig, WasmCaller, WasmConfig, WasmError, WasmExportKind,
    WasmImportKind, WasmInstance, WasmModule, WasmTrap, WasmVal, WasmValType,
};

use crate::builtin_ctx::{BuiltinCtx, NativeValue, heap_string_to_owned};
use crate::{DispatchResult, ExecOutcome, SharedVm, Vm, VmError};

/// Resource tag for compiled [`WasmModule`] handles in [`crate::VmResourceTable`].
pub const TAG_WASM_MODULE: u64 = 0x4E45_5353_5741_534D; // "NESSWASM"
/// Resource tag for live [`WasmInstance`] handles in [`crate::VmResourceTable`].
pub const TAG_WASM_INSTANCE: u64 = 0x4E45_5353_5741_5349; // "NESSWASI"

// Standardized `std.wasm` status codes (`0` = success).
pub const WASM_OK: i64 = 0;
pub const WASM_ERR_VALIDATION: i64 = 1;
pub const WASM_ERR_UNSUPPORTED: i64 = 2;
pub const WASM_ERR_LINK: i64 = 3;
pub const WASM_ERR_UNKNOWN_EXPORT: i64 = 4;
pub const WASM_ERR_SIGNATURE: i64 = 5;
pub const WASM_ERR_MEMORY_LIMIT: i64 = 6;
pub const WASM_ERR_IMMUTABLE_GLOBAL: i64 = 7;
pub const WASM_ERR_OUT_OF_FUEL: i64 = 8;
pub const WASM_ERR_TRAP: i64 = 9;

fn wasm_error_tuple(err: &WasmError) -> (i64, String) {
    let code = match err {
        WasmError::Validation(_) => WASM_ERR_VALIDATION,
        WasmError::UnsupportedFeature(_) => WASM_ERR_UNSUPPORTED,
        WasmError::LinkError(_) => WASM_ERR_LINK,
        WasmError::UnknownExport(_) => WASM_ERR_UNKNOWN_EXPORT,
        WasmError::SignatureMismatch(_) => WASM_ERR_SIGNATURE,
        WasmError::MemoryLimitExceeded(_) => WASM_ERR_MEMORY_LIMIT,
        WasmError::ImmutableGlobal(_) => WASM_ERR_IMMUTABLE_GLOBAL,
        WasmError::Trap(WasmTrap::OutOfFuel) => WASM_ERR_OUT_OF_FUEL,
        WasmError::Trap(_) => WASM_ERR_TRAP,
    };
    (code, err.to_string())
}

// ---------------------------------------------------------------------------
// Embedded `SectionKind::Wasm` / `Opcode::CallWasm` store
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum EmbeddedParamMarshal {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    Bool,
    Char,
    I64,
    U64,
    Isize,
    Usize,
    F32,
    F64,
    StrSlice,
    BytesSlice,
}

#[derive(Clone, Copy, Debug)]
enum EmbeddedRetMarshal {
    Unit,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    Bool,
    Char,
    I64,
    U64,
    Isize,
    Usize,
    F32,
    F64,
}

struct EmbeddedInstanceState {
    instance: WasmInstance,
    allocator_export: Option<String>,
    scratch_base: Option<u32>,
}

struct EmbeddedBindingMeta {
    instance: Arc<Mutex<EmbeddedInstanceState>>,
    export_name: String,
    params: Vec<EmbeddedParamMarshal>,
    ret: EmbeddedRetMarshal,
}

#[derive(Default)]
pub(crate) struct EmbeddedWasmStore {
    bindings: Vec<Arc<EmbeddedBindingMeta>>,
}

enum PreparedWasmArg {
    Val(WasmVal),
    LinearSlice(Vec<u8>),
}

type EmbeddedHostFn =
    Box<dyn FnMut(&mut WasmCaller<'_>, &[WasmVal]) -> Result<Vec<WasmVal>, WasmTrap> + Send>;

fn build_minimal_embedded_wasi_imports(module: &WasmModule) -> HostFuncMap {
    let mut map: HostFuncMap = HashMap::new();
    for imp in module.imports() {
        if !matches!(imp.kind, WasmImportKind::Func(_)) || imp.module != "wasi_snapshot_preview1" {
            continue;
        }
        let name = imp.name.clone();
        let key = (imp.module.clone(), imp.name.clone());
        let cb: EmbeddedHostFn = match name.as_str() {
            "proc_exit" => Box::new(|_caller, args| {
                let code = match args.first() {
                    Some(WasmVal::I32(c)) => *c,
                    _ => 1,
                };
                Err(WasmTrap::Exit(code))
            }),
            "args_sizes_get" | "environ_sizes_get" => Box::new(|caller, args| {
                if let (Some(WasmVal::I32(c_ptr)), Some(WasmVal::I32(b_ptr))) =
                    (args.first(), args.get(1))
                {
                    caller.write_u32_le(*c_ptr as u32, 0)?;
                    caller.write_u32_le(*b_ptr as u32, 0)?;
                }
                Ok(vec![WasmVal::I32(0)])
            }),
            "args_get" | "environ_get" | "sched_yield" | "fd_close" => {
                Box::new(|_caller, _args| Ok(vec![WasmVal::I32(0)]))
            }
            "fd_write" => Box::new(|caller, args| {
                let iovs_ptr = match args.get(1) {
                    Some(WasmVal::I32(v)) => *v as u32,
                    _ => return Ok(vec![WasmVal::I32(28)]),
                };
                let iovs_len = match args.get(2) {
                    Some(WasmVal::I32(v)) => *v as u32,
                    _ => return Ok(vec![WasmVal::I32(28)]),
                };
                let nwritten_ptr = match args.get(3) {
                    Some(WasmVal::I32(v)) => *v as u32,
                    _ => return Ok(vec![WasmVal::I32(28)]),
                };
                let mut total = 0u32;
                for i in 0..iovs_len {
                    let base = iovs_ptr
                        .checked_add(i.checked_mul(8).ok_or(WasmTrap::OutOfBoundsMemoryAccess)?)
                        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
                    let buf_ptr = caller.read_u32_le(base)?;
                    let buf_len = caller.read_u32_le(base + 4)?;
                    let _ = caller.read_bytes(buf_ptr, buf_len)?;
                    total = total.saturating_add(buf_len);
                }
                caller.write_u32_le(nwritten_ptr, total)?;
                Ok(vec![WasmVal::I32(0)])
            }),
            "clock_time_get" => Box::new(|caller, args| {
                if let Some(WasmVal::I32(out_ptr)) = args.get(2) {
                    caller.write_u64_le(*out_ptr as u32, 0)?;
                }
                Ok(vec![WasmVal::I32(0)])
            }),
            "random_get" => Box::new(|caller, args| {
                if let (Some(WasmVal::I32(ptr)), Some(WasmVal::I32(len))) =
                    (args.first(), args.get(1))
                {
                    let zeros = vec![0u8; (*len as u32) as usize];
                    caller.write_bytes(*ptr as u32, &zeros)?;
                }
                Ok(vec![WasmVal::I32(0)])
            }),
            _ => continue,
        };
        map.insert(key, cb);
    }
    map
}

fn find_linear_allocator_export(module: &WasmModule) -> Option<String> {
    for candidate in ["nessa_alloc", "alloc"] {
        if let Some(sig) = module.export_func_sig(candidate)
            && sig.params == [WasmValType::I32]
            && sig.results == [WasmValType::I32]
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

impl Vm {
    /// Validate, instantiate, and install embedded WebAssembly modules from a [`nsbc::CompiledArtifact`].
    pub fn install_wasm_modules(&mut self, modules: &[WasmModuleDef]) -> Result<(), VmError> {
        let _operation = self.operation();
        self.shared.embedded_wasm.write().bindings.clear();
        if modules.is_empty() {
            return Ok(());
        }
        let _ = self.append_wasm_modules(modules)?;
        Ok(())
    }

    /// Incrementally instantiate and append embedded WebAssembly modules during dynamic module loading,
    /// returning the starting binding index (`wasm_binding_base`) assigned to `modules[0]`.
    pub fn append_wasm_modules(&mut self, modules: &[WasmModuleDef]) -> Result<u32, VmError> {
        let _operation = self.operation();
        let current_base = self
            .shared
            .embedded_wasm
            .read(self.worker.worker_id)
            .bindings
            .len() as u32;
        if modules.is_empty() {
            return Ok(current_base);
        }

        let mut resolved_bindings = Vec::new();
        {
            let pool = self.type_pool_read();
            for wmod in modules {
                let parsed = Arc::new(
                    WasmModule::from_bytes(&wmod.wasm_bytes)
                        .map_err(|e| VmError::WasmTrap(e.to_string()))?,
                );
                let allocator_export = find_linear_allocator_export(&parsed);
                let host_imports = build_minimal_embedded_wasi_imports(&parsed);
                let instance = WasmInstance::instantiate(
                    Arc::clone(&parsed),
                    WasmConfig::default(),
                    host_imports,
                    &HashMap::new(),
                )
                .map_err(|e| VmError::WasmTrap(e.to_string()))?;

                let state = Arc::new(Mutex::new(EmbeddedInstanceState {
                    instance,
                    allocator_export,
                    scratch_base: None,
                }));

                for binding in &wmod.bindings {
                    let canon_sig = pool
                        .canonical_type(binding.signature)
                        .ok_or(VmError::TypeError)?;
                    let (param_tys, ret_ty) = match &pool.get(canon_sig).kind {
                        TypeKind::Function { params, ret } => (params.clone(), *ret),
                        _ => return Err(VmError::TypeError),
                    };

                    let mut params = Vec::with_capacity(param_tys.len());
                    for pty in param_tys {
                        let cp = pool.canonical_type(pty).ok_or(VmError::TypeError)?;
                        let marshal = match &pool.get(cp).kind {
                            TypeKind::Intrinsic(intr) => match intr {
                                Intrinsic::I8 => EmbeddedParamMarshal::I8,
                                Intrinsic::U8 => EmbeddedParamMarshal::U8,
                                Intrinsic::I16 => EmbeddedParamMarshal::I16,
                                Intrinsic::U16 => EmbeddedParamMarshal::U16,
                                Intrinsic::I32 => EmbeddedParamMarshal::I32,
                                Intrinsic::U32 => EmbeddedParamMarshal::U32,
                                Intrinsic::Bool => EmbeddedParamMarshal::Bool,
                                Intrinsic::Char => EmbeddedParamMarshal::Char,
                                Intrinsic::I64 => EmbeddedParamMarshal::I64,
                                Intrinsic::U64 => EmbeddedParamMarshal::U64,
                                Intrinsic::Isize => EmbeddedParamMarshal::Isize,
                                Intrinsic::Usize => EmbeddedParamMarshal::Usize,
                                Intrinsic::F32 => EmbeddedParamMarshal::F32,
                                Intrinsic::F64 => EmbeddedParamMarshal::F64,
                                Intrinsic::Str => EmbeddedParamMarshal::StrSlice,
                                _ => return Err(VmError::UnsupportedFunctionAbi),
                            },
                            _ if pool.checked_collection_role(cp).ok().flatten()
                                == Some(CollectionRole::Bytes) =>
                            {
                                EmbeddedParamMarshal::BytesSlice
                            }
                            _ => return Err(VmError::UnsupportedFunctionAbi),
                        };
                        params.push(marshal);
                    }

                    let cr = pool.canonical_type(ret_ty).ok_or(VmError::TypeError)?;
                    let ret = match &pool.get(cr).kind {
                        TypeKind::Intrinsic(intr) => match intr {
                            Intrinsic::Unit => EmbeddedRetMarshal::Unit,
                            Intrinsic::I8 => EmbeddedRetMarshal::I8,
                            Intrinsic::U8 => EmbeddedRetMarshal::U8,
                            Intrinsic::I16 => EmbeddedRetMarshal::I16,
                            Intrinsic::U16 => EmbeddedRetMarshal::U16,
                            Intrinsic::I32 => EmbeddedRetMarshal::I32,
                            Intrinsic::U32 => EmbeddedRetMarshal::U32,
                            Intrinsic::Bool => EmbeddedRetMarshal::Bool,
                            Intrinsic::Char => EmbeddedRetMarshal::Char,
                            Intrinsic::I64 => EmbeddedRetMarshal::I64,
                            Intrinsic::U64 => EmbeddedRetMarshal::U64,
                            Intrinsic::Isize => EmbeddedRetMarshal::Isize,
                            Intrinsic::Usize => EmbeddedRetMarshal::Usize,
                            Intrinsic::F32 => EmbeddedRetMarshal::F32,
                            Intrinsic::F64 => EmbeddedRetMarshal::F64,
                            _ => return Err(VmError::UnsupportedFunctionAbi),
                        },
                        _ => return Err(VmError::UnsupportedFunctionAbi),
                    };

                    resolved_bindings.push(Arc::new(EmbeddedBindingMeta {
                        instance: Arc::clone(&state),
                        export_name: binding.wasm_export.clone(),
                        params,
                        ret,
                    }));
                }
            }
        }

        self.shared
            .embedded_wasm
            .write()
            .bindings
            .extend(resolved_bindings);
        Ok(current_base)
    }

    pub(crate) fn exec_call_wasm(
        &mut self,
        task_id: TaskId,
        binding_id: u32,
        arg_count: u8,
    ) -> DispatchResult {
        let binding = {
            let store = self.shared.embedded_wasm.read(self.worker.worker_id);
            match store.bindings.get(binding_id as usize) {
                Some(b) => Arc::clone(b),
                None => {
                    return DispatchResult::Error(VmError::WasmTrap(format!(
                        "invalid wasm binding id {binding_id}"
                    )));
                }
            }
        };

        if arg_count as usize != binding.params.len() {
            return DispatchResult::Error(VmError::ArityError {
                expected: binding.params.len() as u8,
                got: arg_count,
            });
        }

        let mut raw_args = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
        {
            let Some(task) = self.task_ref(task_id) else {
                return DispatchResult::Error(VmError::InvalidTask);
            };
            raw_args[..arg_count as usize]
                .copy_from_slice(&task.registers.regs[..arg_count as usize]);
        }

        let mut prepared = Vec::with_capacity(arg_count as usize);
        for (val, spec) in raw_args[..arg_count as usize].iter().zip(&binding.params) {
            let item = match self.marshal_embedded_param(*val, *spec) {
                Ok(v) => v,
                Err(err) => return DispatchResult::Error(err),
            };
            prepared.push(item);
        }

        let call_res = self.with_native_frame(|_vm| {
            let mut guard = binding.instance.lock();
            let mut wasm_args = Vec::with_capacity(prepared.len() * 2);
            let mut scratch_cursor = guard.scratch_base;

            for item in &prepared {
                match item {
                    PreparedWasmArg::Val(v) => wasm_args.push(*v),
                    PreparedWasmArg::LinearSlice(bytes) => {
                        let len_u32 = u32::try_from(bytes.len()).map_err(|_| {
                            VmError::WasmTrap("wasm slice argument too large".into())
                        })?;
                        let ptr_u32 = if let Some(alloc_name) = guard.allocator_export.clone() {
                            let res = guard
                                .instance
                                .call_export(&alloc_name, &[WasmVal::I32(len_u32 as i32)])
                                .map_err(|e| VmError::WasmTrap(e.to_string()))?;
                            let Some(WasmVal::I32(p)) = res.first() else {
                                return Err(VmError::WasmTrap(
                                    "wasm allocator returned non-i32 pointer".into(),
                                ));
                            };
                            *p as u32
                        } else {
                            let base = match scratch_cursor {
                                Some(c) => c,
                                None => {
                                    let prev_pages = guard
                                        .instance
                                        .memory_grow(1)
                                        .map_err(|e| VmError::WasmTrap(e.to_string()))?;
                                    let b = prev_pages * 65_536;
                                    guard.scratch_base = Some(b);
                                    b
                                }
                            };
                            let end = base.checked_add(len_u32).ok_or_else(|| {
                                VmError::WasmTrap("wasm linear memory overflow".into())
                            })?;
                            let cur_bytes = guard.instance.memory_pages().saturating_mul(65_536);
                            if end > cur_bytes {
                                let needed_bytes = end - cur_bytes;
                                let extra_pages = needed_bytes.div_ceil(65_536);
                                guard
                                    .instance
                                    .memory_grow(extra_pages)
                                    .map_err(|e| VmError::WasmTrap(e.to_string()))?;
                            }
                            scratch_cursor = Some((end + 7) & !7);
                            base
                        };
                        guard
                            .instance
                            .write_memory(ptr_u32 as usize, bytes)
                            .map_err(|e| VmError::WasmTrap(e.to_string()))?;
                        wasm_args.push(WasmVal::I32(ptr_u32 as i32));
                        wasm_args.push(WasmVal::I32(len_u32 as i32));
                    }
                }
            }

            guard
                .instance
                .call_export(&binding.export_name, &wasm_args)
                .map_err(|e| VmError::WasmTrap(e.to_string()))
        });

        let results = match call_res {
            Ok(r) => r,
            Err(err) => return DispatchResult::Error(err),
        };

        let ret_val = match self.unmarshal_embedded_return(&results, binding.ret) {
            Ok(v) => v,
            Err(err) => return DispatchResult::Error(err),
        };

        let Some(task) = self.task_mut(task_id) else {
            return DispatchResult::Error(VmError::InvalidTask);
        };
        task.registers.set(Reg(0), ret_val);
        DispatchResult::Continue
    }

    fn marshal_embedded_param(
        &self,
        val: TaggedValue,
        spec: EmbeddedParamMarshal,
    ) -> Result<PreparedWasmArg, VmError> {
        // SAFETY: `val` was snapshotted from active task registers under a live mutator session.
        let num = || unsafe { Number::from_tagged(val) }.ok_or(VmError::TypeError);
        match spec {
            EmbeddedParamMarshal::Bool => {
                let b = val.as_bool().ok_or(VmError::TypeError)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(i32::from(b))))
            }
            EmbeddedParamMarshal::Char => {
                let c = val.as_char().ok_or(VmError::TypeError)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(c as u32 as i32)))
            }
            EmbeddedParamMarshal::I8 => {
                let v = i8::try_from(num()?.to_i64_checked().ok_or(VmError::NumericOverflow)?)
                    .map_err(|_| VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(i32::from(v))))
            }
            EmbeddedParamMarshal::U8 => {
                let v = u8::try_from(num()?.to_u64_checked().ok_or(VmError::NumericOverflow)?)
                    .map_err(|_| VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(i32::from(v))))
            }
            EmbeddedParamMarshal::I16 => {
                let v = i16::try_from(num()?.to_i64_checked().ok_or(VmError::NumericOverflow)?)
                    .map_err(|_| VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(i32::from(v))))
            }
            EmbeddedParamMarshal::U16 => {
                let v = u16::try_from(num()?.to_u64_checked().ok_or(VmError::NumericOverflow)?)
                    .map_err(|_| VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(i32::from(v))))
            }
            EmbeddedParamMarshal::I32 => {
                let v = i32::try_from(num()?.to_i64_checked().ok_or(VmError::NumericOverflow)?)
                    .map_err(|_| VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(v)))
            }
            EmbeddedParamMarshal::U32 => {
                let v = u32::try_from(num()?.to_u64_checked().ok_or(VmError::NumericOverflow)?)
                    .map_err(|_| VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I32(v as i32)))
            }
            EmbeddedParamMarshal::I64 | EmbeddedParamMarshal::Isize => {
                let v = num()?.to_i64_checked().ok_or(VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I64(v)))
            }
            EmbeddedParamMarshal::U64 | EmbeddedParamMarshal::Usize => {
                let v = num()?.to_u64_checked().ok_or(VmError::NumericOverflow)?;
                Ok(PreparedWasmArg::Val(WasmVal::I64(v as i64)))
            }
            EmbeddedParamMarshal::F32 => {
                let v = num()?.to_f64() as f32;
                Ok(PreparedWasmArg::Val(WasmVal::f32(v)))
            }
            EmbeddedParamMarshal::F64 => {
                let v = num()?.to_f64();
                Ok(PreparedWasmArg::Val(WasmVal::f64(v)))
            }
            EmbeddedParamMarshal::StrSlice => {
                let slice = crate::bytes_obj::heap_str_slice(val).ok_or(VmError::TypeError)?;
                Ok(PreparedWasmArg::LinearSlice(slice.to_vec()))
            }
            EmbeddedParamMarshal::BytesSlice => {
                let layout = self.bytes_layout(val)?;
                Ok(PreparedWasmArg::LinearSlice(layout.as_slice().to_vec()))
            }
        }
    }

    fn unmarshal_embedded_return(
        &mut self,
        results: &[WasmVal],
        spec: EmbeddedRetMarshal,
    ) -> Result<TaggedValue, VmError> {
        match spec {
            EmbeddedRetMarshal::Unit => Ok(TaggedValue::UNIT),
            EmbeddedRetMarshal::Bool => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                Ok(TaggedValue::from_bool(*v != 0))
            }
            EmbeddedRetMarshal::Char => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                let ch = char::from_u32(*v as u32).ok_or_else(|| {
                    VmError::WasmTrap(format!("invalid char scalar {:#x}", *v as u32))
                })?;
                Ok(TaggedValue::from_char(ch))
            }
            EmbeddedRetMarshal::I8 => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::I64(i64::from(*v as i8)), Intrinsic::I8)
            }
            EmbeddedRetMarshal::U8 => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::U64(u64::from(*v as u8)), Intrinsic::U8)
            }
            EmbeddedRetMarshal::I16 => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::I64(i64::from(*v as i16)), Intrinsic::I16)
            }
            EmbeddedRetMarshal::U16 => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::U64(u64::from(*v as u16)), Intrinsic::U16)
            }
            EmbeddedRetMarshal::I32 => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::I64(i64::from(*v)), Intrinsic::I32)
            }
            EmbeddedRetMarshal::U32 => {
                let Some(WasmVal::I32(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::U64(u64::from(*v as u32)), Intrinsic::U32)
            }
            EmbeddedRetMarshal::I64 => {
                let Some(WasmVal::I64(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.number_value(Number::I64(*v))
            }
            EmbeddedRetMarshal::U64 => {
                let Some(WasmVal::I64(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.number_value(Number::U64(*v as u64))
            }
            EmbeddedRetMarshal::Isize => {
                let Some(WasmVal::I64(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::I64(*v), Intrinsic::Isize)
            }
            EmbeddedRetMarshal::Usize => {
                let Some(WasmVal::I64(v)) = results.first() else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::U64(*v as u64), Intrinsic::Usize)
            }
            EmbeddedRetMarshal::F32 => {
                let Some(v) = results.first().and_then(|x| x.as_f32()) else {
                    return Err(VmError::TypeError);
                };
                self.typed_number_value(Number::F64(f64::from(v)), Intrinsic::F32)
            }
            EmbeddedRetMarshal::F64 => {
                let Some(v) = results.first().and_then(|x| x.as_f64()) else {
                    return Err(VmError::TypeError);
                };
                self.number_value(Number::F64(v))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Synchronous Nessa closure callback execution for `std.wasm` host imports
    // -----------------------------------------------------------------------

    fn invoke_host_import_closure(
        &mut self,
        task_id: TaskId,
        inst_id: i64,
        slot_idx: usize,
        sig: &FuncSig,
        wasm_args: &[WasmVal],
    ) -> Result<Vec<WasmVal>, VmError> {
        let closure_val = {
            let guard = self.roots().wasm_host_imports.lock();
            guard
                .get(&inst_id)
                .and_then(|slots| slots.get(slot_idx))
                .copied()
                .ok_or(VmError::InvalidArgument("unbound wasm host import closure"))?
        };

        let saved_regs = {
            let task = self.task_ref(task_id).ok_or(VmError::InvalidTask)?;
            task.registers.regs
        };

        let root_base = self.worker_roots().temporary_values.borrow().len();
        let mut temp_slots = Vec::with_capacity(runtime::GP_REGISTER_COUNT + 1 + wasm_args.len());
        temp_slots.extend_from_slice(&saved_regs);
        temp_slots.push(closure_val);
        temp_slots.resize(
            runtime::GP_REGISTER_COUNT + 1 + wasm_args.len(),
            TaggedValue::UNIT,
        );

        self.with_temporary_roots(&temp_slots, |vm| {
            for (idx, wv) in wasm_args.iter().enumerate() {
                let tv = vm.wasm_val_to_tagged(*wv)?;
                vm.worker_roots().temporary_values.borrow_mut()
                    [root_base + runtime::GP_REGISTER_COUNT + 1 + idx] = tv;
            }

            let (reloaded_closure, nessa_args) = {
                let roots = vm.worker_roots().temporary_values.borrow();
                let c = roots[root_base + runtime::GP_REGISTER_COUNT];
                let a = roots[root_base + runtime::GP_REGISTER_COUNT + 1
                    ..root_base + runtime::GP_REGISTER_COUNT + 1 + wasm_args.len()]
                    .to_vec();
                (c, a)
            };

            let layout = Self::closure_layout(reloaded_closure)?;
            let func_id = layout.function;
            let base_depth = vm
                .task_ref(task_id)
                .ok_or(VmError::InvalidTask)?
                .call_stack
                .len();

            let ret_tagged_res: Result<TaggedValue, VmError> =
                match vm.invoke_closure(task_id, func_id, Some(reloaded_closure), &nessa_args) {
                    DispatchResult::Return(v) => Ok(v),
                    DispatchResult::Error(err) => Err(err),
                    DispatchResult::Yield
                    | DispatchResult::SuspendRetry
                    | DispatchResult::StartCancelUnwind
                    | DispatchResult::ResumeCancelUnwind => Err(VmError::EffectAcrossNativeFrame),
                    DispatchResult::Continue => vm.step_nested_host_callback(task_id, base_depth),
                };

            // Unwind any remaining frames above `base_depth` if an error occurred inside the callback,
            // then restore caller's exact `r0..r31` registers from GC-updated temporary roots.
            if ret_tagged_res.is_err() {
                vm.unwind_frames_to_depth(task_id, base_depth);
            }
            {
                let restored: [TaggedValue; runtime::GP_REGISTER_COUNT] = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    roots[root_base..root_base + runtime::GP_REGISTER_COUNT]
                        .try_into()
                        .expect("exact 32 register root slice")
                };
                if let Some(task) = vm.task_mut(task_id) {
                    task.registers.regs = restored;
                }
            }

            let ret_tagged = ret_tagged_res?;
            vm.tagged_to_wasm_results(ret_tagged, &sig.results)
        })
    }

    fn step_nested_host_callback(
        &mut self,
        task_id: TaskId,
        base_depth: usize,
    ) -> Result<TaggedValue, VmError> {
        let mut validated_func: Option<nsbc::FuncId> = None;
        loop {
            let (func_id, pc, depth) = {
                let task = self.task_ref(task_id).ok_or(VmError::InvalidTask)?;
                (task.current_func, task.pc, task.call_stack.len())
            };
            if depth <= base_depth {
                let task = self.task_ref(task_id).ok_or(VmError::InvalidTask)?;
                return Ok(task.registers.get(Reg(0)));
            }
            let func_len = if validated_func == Some(func_id) {
                self.shared
                    .bytecode
                    .try_get_function(func_id)
                    .ok_or(VmError::InvalidFunction(func_id))?
                    .instructions
                    .len()
            } else {
                let f = self.checked_function(func_id)?;
                validated_func = Some(func_id);
                f.instructions.len()
            };
            if pc as usize >= func_len {
                if let Some(outcome) =
                    self.complete_return(task_id, pc, TaggedValue::UNIT, &mut validated_func)
                {
                    return match outcome {
                        ExecOutcome::Error(e) => Err(e),
                        _ => Ok(TaggedValue::UNIT),
                    };
                }
                if self
                    .task_ref(task_id)
                    .is_some_and(|t| t.call_stack.len() <= base_depth)
                {
                    return Ok(TaggedValue::UNIT);
                }
                continue;
            }

            let instr = match self.shared.bytecode.get_meta(func_id).decoded[pc as usize] {
                Some(i) => i,
                None => {
                    let word = self.shared.bytecode.get_function(func_id).instructions[pc as usize];
                    return Err(VmError::InvalidInstruction(word));
                }
            };

            if let Some(task) = self.task_mut(task_id) {
                task.pc = pc + 1;
            }

            match self.dispatch(task_id, instr) {
                DispatchResult::Continue => {}
                DispatchResult::Yield => {
                    if self.scheduler().is_aborted() {
                        return Err(VmError::TaskCancelled);
                    }
                }
                DispatchResult::Return(val) => {
                    let returning_to_wasm = self
                        .task_ref(task_id)
                        .is_some_and(|t| t.call_stack.len() == base_depth + 1);
                    if let Some(outcome) =
                        self.complete_return(task_id, pc, val, &mut validated_func)
                    {
                        return match outcome {
                            ExecOutcome::Error(e) => Err(e),
                            _ => Ok(val),
                        };
                    }
                    if returning_to_wasm {
                        return Ok(val);
                    }
                }
                DispatchResult::SuspendRetry => return Err(VmError::EffectAcrossNativeFrame),
                DispatchResult::StartCancelUnwind | DispatchResult::ResumeCancelUnwind => {
                    return Err(VmError::TaskCancelled);
                }
                DispatchResult::Error(err) => return Err(err),
            }
        }
    }

    fn unwind_frames_to_depth(&mut self, task_id: TaskId, base_depth: usize) {
        let mut recycled = Vec::new();
        if let Some(task) = self.task_mut(task_id) {
            while task.call_stack.len() > base_depth {
                if let Some(frame) = task.call_stack.pop() {
                    task.pc = frame.return_pc;
                    task.current_func = frame.func_id;
                    recycled.push(std::mem::replace(&mut task.local_slots, frame.local_slots));
                    task.display_state = frame.display_state;
                    task.unwind_stack = frame.unwind_stack;
                    task.is_unwind_target = frame.is_unwind_target;
                    for (reg, saved_val) in &frame.saved_regs {
                        task.registers.set(*reg, *saved_val);
                    }
                }
            }
        }
        for slots in recycled {
            self.recycle_frame_slots(slots);
        }
    }

    fn wasm_val_to_tagged(&mut self, val: WasmVal) -> Result<TaggedValue, VmError> {
        match val {
            WasmVal::I32(v) => self.number_value(Number::I64(i64::from(v))),
            WasmVal::I64(v) => self.number_value(Number::I64(v)),
            WasmVal::F32(bits) => self.number_value(Number::F64(f64::from(f32::from_bits(bits)))),
            WasmVal::F64(bits) => self.number_value(Number::F64(f64::from_bits(bits))),
            WasmVal::V128(lanes) => self.alloc_bytes_from_slice(&lanes.to_le_bytes()),
            WasmVal::FuncRef(Some(idx)) => self.number_value(Number::I64(i64::from(idx))),
            WasmVal::FuncRef(None) | WasmVal::ExternRef(None) => Ok(TaggedValue::NULL),
            WasmVal::ExternRef(Some(id)) => self.number_value(Number::U64(id)),
        }
    }

    fn wasm_val_to_native(&self, val: WasmVal) -> NativeValue {
        match val {
            WasmVal::I32(v) => NativeValue::I64(i64::from(v)),
            WasmVal::I64(v) => NativeValue::I64(v),
            WasmVal::F32(bits) => NativeValue::F64(f64::from(f32::from_bits(bits))),
            WasmVal::F64(bits) => NativeValue::F64(f64::from_bits(bits)),
            WasmVal::V128(lanes) => NativeValue::Bytes(lanes.to_le_bytes().to_vec()),
            WasmVal::FuncRef(Some(idx)) => NativeValue::I64(i64::from(idx)),
            WasmVal::FuncRef(None) | WasmVal::ExternRef(None) => NativeValue::Null,
            WasmVal::ExternRef(Some(id)) => NativeValue::U64(id),
        }
    }

    fn tagged_to_wasm_val(&self, val: TaggedValue, ty: WasmValType) -> Result<WasmVal, WasmError> {
        // SAFETY: `val` is a live VM value read synchronously under a mutator session.
        let num_opt = unsafe { Number::from_tagged(val) };
        match ty {
            WasmValType::I32 => {
                if let Some(b) = val.as_bool() {
                    return Ok(WasmVal::I32(i32::from(b)));
                }
                if let Some(c) = val.as_char() {
                    return Ok(WasmVal::I32(c as u32 as i32));
                }
                let n = num_opt.ok_or_else(|| {
                    WasmError::SignatureMismatch("expected integer/bool/char for wasm i32".into())
                })?;
                if let Some(i) = n.to_i64_checked() {
                    Ok(WasmVal::I32(i as i32))
                } else if let Some(u) = n.to_u64_checked() {
                    Ok(WasmVal::I32(u as u32 as i32))
                } else {
                    Err(WasmError::SignatureMismatch(
                        "integer out of range for wasm i32".into(),
                    ))
                }
            }
            WasmValType::I64 => {
                if let Some(b) = val.as_bool() {
                    return Ok(WasmVal::I64(i64::from(b)));
                }
                let n = num_opt.ok_or_else(|| {
                    WasmError::SignatureMismatch("expected integer for wasm i64".into())
                })?;
                if let Some(i) = n.to_i64_checked() {
                    Ok(WasmVal::I64(i))
                } else if let Some(u) = n.to_u64_checked() {
                    Ok(WasmVal::I64(u as i64))
                } else {
                    Err(WasmError::SignatureMismatch(
                        "integer out of range for wasm i64".into(),
                    ))
                }
            }
            WasmValType::F32 => {
                let n = num_opt.ok_or_else(|| {
                    WasmError::SignatureMismatch("expected number for wasm f32".into())
                })?;
                Ok(WasmVal::f32(n.to_f64() as f32))
            }
            WasmValType::F64 => {
                let n = num_opt.ok_or_else(|| {
                    WasmError::SignatureMismatch("expected number for wasm f64".into())
                })?;
                Ok(WasmVal::f64(n.to_f64()))
            }
            WasmValType::V128 => {
                if let Ok(layout) = self.bytes_layout(val) {
                    let s = layout.as_slice();
                    if s.len() != 16 {
                        return Err(WasmError::SignatureMismatch(format!(
                            "wasm v128 Bytes argument must be exactly 16 bytes, got {}",
                            s.len()
                        )));
                    }
                    let arr: [u8; 16] = s.try_into().unwrap();
                    return Ok(WasmVal::V128(u128::from_le_bytes(arr)));
                }
                if let Some(Number::U128(v)) = num_opt {
                    return Ok(WasmVal::V128(v));
                }
                if let Some(Number::I128(v)) = num_opt {
                    return Ok(WasmVal::V128(v as u128));
                }
                Err(WasmError::SignatureMismatch(
                    "expected 16-byte Bytes or u128/i128 for wasm v128".into(),
                ))
            }
            WasmValType::FuncRef => {
                if val.is_null() {
                    return Ok(WasmVal::FuncRef(None));
                }
                let idx = num_opt
                    .and_then(|n| n.to_u64_checked())
                    .and_then(|u| u32::try_from(u).ok())
                    .ok_or_else(|| {
                        WasmError::SignatureMismatch("expected ?u32 for wasm funcref".into())
                    })?;
                Ok(WasmVal::FuncRef(Some(idx)))
            }
            WasmValType::ExternRef => {
                if val.is_null() {
                    return Ok(WasmVal::ExternRef(None));
                }
                let id = num_opt.and_then(|n| n.to_u64_checked()).ok_or_else(|| {
                    WasmError::SignatureMismatch("expected ?u64 for wasm externref".into())
                })?;
                Ok(WasmVal::ExternRef(Some(id)))
            }
        }
    }

    fn tagged_to_wasm_results(
        &self,
        val: TaggedValue,
        expected: &[WasmValType],
    ) -> Result<Vec<WasmVal>, VmError> {
        match expected {
            [] => Ok(Vec::new()),
            [single] => self
                .tagged_to_wasm_val(val, *single)
                .map(|v| vec![v])
                .map_err(|e| VmError::WasmTrap(e.to_string())),
            many => {
                let elems = self.tuple_values(val)?;
                if elems.len() != many.len() {
                    return Err(VmError::WasmTrap(format!(
                        "host import expected {}-element tuple return, got {}",
                        many.len(),
                        elems.len()
                    )));
                }
                let mut out = Vec::with_capacity(many.len());
                for (elem, ty) in elems.into_iter().zip(many.iter()) {
                    let wv = self
                        .tagged_to_wasm_val(elem, *ty)
                        .map_err(|e| VmError::WasmTrap(e.to_string()))?;
                    out.push(wv);
                }
                Ok(out)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Managed resource payloads for `std.wasm`
// ---------------------------------------------------------------------------

struct ManagedWasmModule {
    module: Arc<WasmModule>,
}

struct ManagedWasmInstance {
    inst_id: i64,
    module: Arc<WasmModule>,
    instance: Arc<Mutex<Option<WasmInstance>>>,
    dynamic_imports: Arc<HashMap<(String, String), (usize, FuncSig)>>,
}

struct InstanceCheckoutGuard {
    slot: Arc<Mutex<Option<WasmInstance>>>,
    inst: Option<WasmInstance>,
}

impl InstanceCheckoutGuard {
    fn checkout(slot: &Arc<Mutex<Option<WasmInstance>>>) -> Result<Self, WasmError> {
        let inst = slot.lock().take().ok_or_else(|| {
            WasmError::Trap(WasmTrap::HostTrap(
                "reentrant call on active WasmInstance is not permitted".into(),
            ))
        })?;
        Ok(Self {
            slot: Arc::clone(slot),
            inst: Some(inst),
        })
    }

    fn as_mut(&mut self) -> &mut WasmInstance {
        self.inst
            .as_mut()
            .expect("checked-out WasmInstance present")
    }
}

impl Drop for InstanceCheckoutGuard {
    fn drop(&mut self) {
        if let Some(inst) = self.inst.take() {
            *self.slot.lock() = Some(inst);
        }
    }
}

fn format_func_sig(sig: &FuncSig) -> String {
    let params: Vec<&str> = sig.params.iter().map(|p| p.as_str()).collect();
    let results: Vec<&str> = sig.results.iter().map(|r| r.as_str()).collect();
    let ret_part = match results.as_slice() {
        [] => "()".to_owned(),
        [one] => (*one).to_owned(),
        many => format!("({})", many.join(", ")),
    };
    format!("({}) -> {ret_part}", params.join(", "))
}

// ---------------------------------------------------------------------------
// `std.wasm` Builtin Implementations (`ids::WASM_*` 865..=882)
// ---------------------------------------------------------------------------

/// `__wasm_validate(bytes: Bytes) -> (i64, String)`
pub fn wasm_validate(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let bytes_arg = ctx.arg(0)?;
    let res = ctx.with_bytes_slice(bytes_arg, |slice| Ok(wasm::validate_wasm_bytes(slice)))?;
    match res {
        Ok(()) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
            ]))
        }
    }
}

/// `__wasm_inspect(bytes: Bytes) -> (i64, String, String)`
pub fn wasm_inspect(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let bytes_arg = ctx.arg(0)?;
    let res = ctx.with_bytes_slice(bytes_arg, |slice| Ok(wasm::inspect_wasm_bytes(slice)))?;
    match res {
        Ok(summary) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
            NativeValue::Str(summary),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::Str(String::new()),
            ]))
        }
    }
}

/// `__wasm_compile(bytes: Bytes) -> (i64, String, i64)`
pub fn wasm_compile(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let bytes_arg = ctx.arg(0)?;
    let parsed = ctx.with_bytes_slice(bytes_arg, |slice| Ok(WasmModule::from_bytes(slice)))?;
    match parsed {
        Ok(module) => {
            let id = ctx.resources_mut().insert(
                TAG_WASM_MODULE,
                ManagedWasmModule {
                    module: Arc::new(module),
                },
            )?;
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(WASM_OK),
                NativeValue::Str(String::new()),
                NativeValue::I64(id),
            ]))
        }
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::I64(-1),
            ]))
        }
    }
}

/// `__wasm_module_exports(mod_id: i64) -> List<(String, String, String)>`
pub fn wasm_module_exports(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let mod_id = ctx.arg_i64(0)?;
    let module = {
        let table = ctx.resources();
        let entry = table.get::<ManagedWasmModule>(mod_id, TAG_WASM_MODULE)?;
        Arc::clone(&entry.module)
    };
    let mut items = Vec::new();
    for exp in module.exports() {
        let (kind_str, detail) = match exp.kind {
            WasmExportKind::Func(sig) => ("func", format_func_sig(&sig)),
            WasmExportKind::Memory {
                min_pages,
                max_pages,
            } => (
                "memory",
                match max_pages {
                    Some(max) => format!("{min_pages}..{max} pages"),
                    None => format!("{min_pages}.. pages"),
                },
            ),
            WasmExportKind::Table { element, min, max } => (
                "table",
                match max {
                    Some(m) => format!("{element} {min}..{m}"),
                    None => format!("{element} {min}.."),
                },
            ),
            WasmExportKind::Global { val_type, mutable } => (
                "global",
                if mutable {
                    format!("mut {val_type}")
                } else {
                    format!("const {val_type}")
                },
            ),
        };
        items.push(NativeValue::Tuple(vec![
            NativeValue::Str(exp.name),
            NativeValue::Str(kind_str.to_owned()),
            NativeValue::Str(detail),
        ]));
    }
    ctx.return_native(NativeValue::List(items))
}

/// `__wasm_module_imports(mod_id: i64) -> List<(String, String, String, String)>`
pub fn wasm_module_imports(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let mod_id = ctx.arg_i64(0)?;
    let module = {
        let table = ctx.resources();
        let entry = table.get::<ManagedWasmModule>(mod_id, TAG_WASM_MODULE)?;
        Arc::clone(&entry.module)
    };
    let mut items = Vec::new();
    for imp in module.imports() {
        let (kind_str, detail) = match &imp.kind {
            WasmImportKind::Func(sig) => ("func", format_func_sig(sig)),
            WasmImportKind::Memory {
                min_pages,
                max_pages,
            } => (
                "memory",
                match max_pages {
                    Some(max) => format!("{min_pages}..{max} pages"),
                    None => format!("{min_pages}.. pages"),
                },
            ),
            WasmImportKind::Table { element, min, max } => (
                "table",
                match max {
                    Some(m) => format!("{element} {min}..{m}"),
                    None => format!("{element} {min}.."),
                },
            ),
            WasmImportKind::Global { val_type, mutable } => (
                "global",
                if *mutable {
                    format!("mut {val_type}")
                } else {
                    format!("const {val_type}")
                },
            ),
        };
        items.push(NativeValue::Tuple(vec![
            NativeValue::Str(imp.module.clone()),
            NativeValue::Str(imp.name.clone()),
            NativeValue::Str(kind_str.to_owned()),
            NativeValue::Str(detail),
        ]));
    }
    ctx.return_native(NativeValue::List(items))
}

/// `__wasm_instantiate(mod_id: i64, imports: Map<String, Any>, max_pages: i64, max_depth: i64, fuel: i64) -> (i64, String, i64)`
pub fn wasm_instantiate(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(5)?;
    let mod_id = ctx.arg_i64(0)?;
    let imports_map = ctx.arg(1)?;
    let max_pages_arg = ctx.arg_i64(2)?;
    let max_depth_arg = ctx.arg_i64(3)?;
    let fuel_arg = ctx.arg_i64(4)?;

    let module = {
        let table = ctx.resources();
        let entry = table.get::<ManagedWasmModule>(mod_id, TAG_WASM_MODULE)?;
        Arc::clone(&entry.module)
    };

    let raw_entries = {
        let _guard = ctx.vm.stripe_read(imports_map)?;
        let layout = ctx.vm.map_layout_unlocked(imports_map)?;
        layout.snapshot_entries()?
    };

    let lookup_import_val = |mod_name: &str, item_name: &str| -> Option<TaggedValue> {
        let qualified = format!("{mod_name}::{item_name}");
        for (k, v) in &raw_entries {
            if k == &qualified || (mod_name == "env" && k == item_name) {
                return Some(*v);
            }
        }
        None
    };

    let mut dynamic_keys = HashSet::new();
    let mut dynamic_meta = HashMap::new();
    let mut rooted_closures = Vec::new();
    let mut imported_globals = HashMap::new();

    for imp in module.imports() {
        match &imp.kind {
            WasmImportKind::Func(sig) => {
                let Some(val) = lookup_import_val(&imp.module, &imp.name) else {
                    let err = WasmError::LinkError(format!(
                        "missing required host function import `{}::{}`",
                        imp.module, imp.name
                    ));
                    let (code, msg) = wasm_error_tuple(&err);
                    return ctx.return_native(NativeValue::Tuple(vec![
                        NativeValue::I64(code),
                        NativeValue::Str(msg),
                        NativeValue::I64(-1),
                    ]));
                };
                if Vm::closure_layout(val).is_err() {
                    let err = WasmError::LinkError(format!(
                        "host function import `{}::{}` must be a callable function/closure",
                        imp.module, imp.name
                    ));
                    let (code, msg) = wasm_error_tuple(&err);
                    return ctx.return_native(NativeValue::Tuple(vec![
                        NativeValue::I64(code),
                        NativeValue::Str(msg),
                        NativeValue::I64(-1),
                    ]));
                }
                let slot_idx = rooted_closures.len();
                rooted_closures.push(val);
                let key = (imp.module.clone(), imp.name.clone());
                dynamic_keys.insert(key.clone());
                dynamic_meta.insert(key, (slot_idx, sig.clone()));
            }
            WasmImportKind::Global { val_type, .. } => {
                let Some(val) = lookup_import_val(&imp.module, &imp.name) else {
                    let err = WasmError::LinkError(format!(
                        "missing required global import `{}::{}`",
                        imp.module, imp.name
                    ));
                    let (code, msg) = wasm_error_tuple(&err);
                    return ctx.return_native(NativeValue::Tuple(vec![
                        NativeValue::I64(code),
                        NativeValue::Str(msg),
                        NativeValue::I64(-1),
                    ]));
                };
                match ctx.vm.tagged_to_wasm_val(val, *val_type) {
                    Ok(wv) => {
                        imported_globals.insert((imp.module.clone(), imp.name.clone()), wv);
                    }
                    Err(err) => {
                        let (code, msg) = wasm_error_tuple(&err);
                        return ctx.return_native(NativeValue::Tuple(vec![
                            NativeValue::I64(code),
                            NativeValue::Str(msg),
                            NativeValue::I64(-1),
                        ]));
                    }
                }
            }
            WasmImportKind::Memory { .. } | WasmImportKind::Table { .. } => {}
        }
    }

    let config = WasmConfig {
        max_memory_pages: if max_pages_arg <= 0 {
            1024
        } else {
            u32::try_from(max_pages_arg).unwrap_or(u32::MAX)
        },
        max_call_depth: if max_depth_arg <= 0 {
            1024
        } else {
            usize::try_from(max_depth_arg).unwrap_or(1024)
        },
        fuel_limit: if fuel_arg < 0 {
            None
        } else {
            Some(fuel_arg as u64)
        },
    };

    let instance_slot: Arc<Mutex<Option<WasmInstance>>> = Arc::new(Mutex::new(None));
    let dynamic_arc = Arc::new(dynamic_meta);
    let weak_shared = Arc::downgrade(&ctx.vm.shared);

    // Reserve resource handle ID up front so even a Wasm `start` section can invoke GC-rooted host closures.
    let inst_id = ctx.resources_mut().insert_with_cleanup(
        TAG_WASM_INSTANCE,
        ManagedWasmInstance {
            inst_id: 0,
            module: Arc::clone(&module),
            instance: Arc::clone(&instance_slot),
            dynamic_imports: Arc::clone(&dynamic_arc),
        },
        move |managed: ManagedWasmInstance| {
            let shared_opt: Option<Arc<SharedVm>> = weak_shared.upgrade();
            if let Some(shared) = shared_opt {
                shared
                    .roots
                    .wasm_host_imports
                    .lock()
                    .remove(&managed.inst_id);
            }
        },
    )?;
    if let Ok(entry) = ctx
        .resources_mut()
        .get_mut::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)
    {
        entry.inst_id = inst_id;
    }
    if !rooted_closures.is_empty() {
        ctx.vm
            .roots()
            .wasm_host_imports
            .lock()
            .insert(inst_id, rooted_closures);
    }

    let task_id = ctx.task_id;
    let mut pending_vm_error: Option<VmError> = None;
    let inst_res = ctx.vm.with_native_frame(|vm| {
        let mut disp = |_caller: &mut WasmCaller<'_>,
                        mod_name: &str,
                        item_name: &str,
                        args: &[WasmVal]|
         -> Result<Vec<WasmVal>, WasmTrap> {
            let key = (mod_name.to_owned(), item_name.to_owned());
            let Some((slot_idx, sig)) = dynamic_arc.get(&key) else {
                return Err(WasmTrap::HostTrap(format!(
                    "unbound host import `{mod_name}::{item_name}`"
                )));
            };
            match vm.invoke_host_import_closure(task_id, inst_id, *slot_idx, sig, args) {
                Ok(vals) => Ok(vals),
                Err(vm_err) => {
                    let msg = vm_err.to_string();
                    pending_vm_error = Some(vm_err);
                    Err(WasmTrap::HostTrap(msg))
                }
            }
        };
        Ok(WasmInstance::instantiate_with_dynamic_imports(
            Arc::clone(&module),
            config,
            HashMap::new(),
            &dynamic_keys,
            &imported_globals,
            Some(&mut disp),
        ))
    })?;

    if let Some(vm_err) = pending_vm_error {
        let _ = ctx.resources_mut().close(inst_id, Some(TAG_WASM_INSTANCE));
        ctx.vm.roots().wasm_host_imports.lock().remove(&inst_id);
        return Err(vm_err);
    }

    match inst_res {
        Ok(inst) => {
            *instance_slot.lock() = Some(inst);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(WASM_OK),
                NativeValue::Str(String::new()),
                NativeValue::I64(inst_id),
            ]))
        }
        Err(err) => {
            let _ = ctx.resources_mut().close(inst_id, Some(TAG_WASM_INSTANCE));
            ctx.vm.roots().wasm_host_imports.lock().remove(&inst_id);
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::I64(-1),
            ]))
        }
    }
}

/// `__wasm_call(inst_id: i64, export_name: String, args: List<Any>) -> (i64, String, List<Any>)`
pub fn wasm_call(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let inst_id = ctx.arg_i64(0)?;
    let export_name = ctx.arg_string(1)?;
    let args_list = ctx.arg(2)?;

    let (module, instance_slot, dynamic_arc) = {
        let table = ctx.resources();
        let entry = table.get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?;
        (
            Arc::clone(&entry.module),
            Arc::clone(&entry.instance),
            Arc::clone(&entry.dynamic_imports),
        )
    };

    let Some(sig) = module.export_func_sig(&export_name).cloned() else {
        let err = WasmError::UnknownExport(export_name);
        let (code, msg) = wasm_error_tuple(&err);
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(code),
            NativeValue::Str(msg),
            NativeValue::List(Vec::new()),
        ]));
    };

    let raw_args = ctx.vm.snapshot_list_elements(args_list)?;
    if raw_args.len() != sig.params.len() {
        let err = WasmError::SignatureMismatch(format!(
            "export `{export_name}` expects {} arguments, got {}",
            sig.params.len(),
            raw_args.len()
        ));
        let (code, msg) = wasm_error_tuple(&err);
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(code),
            NativeValue::Str(msg),
            NativeValue::List(Vec::new()),
        ]));
    }

    let mut wasm_args = Vec::with_capacity(sig.params.len());
    for (val, expected_ty) in raw_args.into_iter().zip(sig.params.iter()) {
        match ctx.vm.tagged_to_wasm_val(val, *expected_ty) {
            Ok(wv) => wasm_args.push(wv),
            Err(err) => {
                let (code, msg) = wasm_error_tuple(&err);
                return ctx.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(code),
                    NativeValue::Str(msg),
                    NativeValue::List(Vec::new()),
                ]));
            }
        }
    }

    let mut checkout = match InstanceCheckoutGuard::checkout(&instance_slot) {
        Ok(g) => g,
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            return ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::List(Vec::new()),
            ]));
        }
    };

    let task_id = ctx.task_id;
    let mut pending_vm_error: Option<VmError> = None;
    let call_res = ctx.vm.with_native_frame(|vm| {
        let mut disp = |_caller: &mut WasmCaller<'_>,
                        mod_name: &str,
                        item_name: &str,
                        cb_args: &[WasmVal]|
         -> Result<Vec<WasmVal>, WasmTrap> {
            let key = (mod_name.to_owned(), item_name.to_owned());
            let Some((slot_idx, cb_sig)) = dynamic_arc.get(&key) else {
                return Err(WasmTrap::HostTrap(format!(
                    "unbound host import `{mod_name}::{item_name}`"
                )));
            };
            match vm.invoke_host_import_closure(task_id, inst_id, *slot_idx, cb_sig, cb_args) {
                Ok(vals) => Ok(vals),
                Err(vm_err) => {
                    let msg = vm_err.to_string();
                    pending_vm_error = Some(vm_err);
                    Err(WasmTrap::HostTrap(msg))
                }
            }
        };
        Ok(checkout
            .as_mut()
            .call_export_with_dispatcher(&export_name, &wasm_args, Some(&mut disp)))
    })?;
    drop(checkout);

    if let Some(vm_err) = pending_vm_error {
        return Err(vm_err);
    }

    match call_res {
        Ok(vals) => {
            let items: Vec<NativeValue> = vals
                .into_iter()
                .map(|v| ctx.vm.wasm_val_to_native(v))
                .collect();
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(WASM_OK),
                NativeValue::Str(String::new()),
                NativeValue::List(items),
            ]))
        }
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::List(Vec::new()),
            ]))
        }
    }
}

/// `__wasm_mem_pages(inst_id: i64) -> i64`
pub fn wasm_mem_pages(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let inst_id = ctx.arg_i64(0)?;
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let guard = slot.lock();
    let inst = guard.as_ref().ok_or(VmError::InvalidArgument(
        "wasm instance is currently active",
    ))?;
    ctx.return_i64(i64::from(inst.memory_pages()))
}

/// `__wasm_mem_grow(inst_id: i64, delta_pages: i64) -> (i64, String, i64)`
pub fn wasm_mem_grow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let inst_id = ctx.arg_i64(0)?;
    let delta = ctx.arg_i64(1)?;
    let Ok(delta_u32) = u32::try_from(delta) else {
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_ERR_MEMORY_LIMIT),
            NativeValue::Str("delta_pages must be in 0..=u32::MAX".into()),
            NativeValue::I64(-1),
        ]));
    };
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let res = {
        let mut guard = slot.lock();
        let inst = guard.as_mut().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        inst.memory_grow(delta_u32)
    };
    match res {
        Ok(prev) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
            NativeValue::I64(i64::from(prev)),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::I64(-1),
            ]))
        }
    }
}

/// `__wasm_mem_read(inst_id: i64, offset: i64, len: i64) -> (i64, String, Bytes)`
pub fn wasm_mem_read(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let inst_id = ctx.arg_i64(0)?;
    let offset = ctx.arg_i64(1)?;
    let len = ctx.arg_i64(2)?;
    if offset < 0 || len < 0 {
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_ERR_TRAP),
            NativeValue::Str("wasm memory read offset/len must be >= 0".into()),
            NativeValue::Bytes(Vec::new()),
        ]));
    }
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let read_res = {
        let guard = slot.lock();
        let inst = guard.as_ref().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        inst.read_memory(offset as usize, len as usize)
            .map(|s| s.to_vec())
    };
    match read_res {
        Ok(data) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
            NativeValue::Bytes(data),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::Bytes(Vec::new()),
            ]))
        }
    }
}

/// `__wasm_mem_write(inst_id: i64, offset: i64, data: Bytes) -> (i64, String)`
pub fn wasm_mem_write(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let inst_id = ctx.arg_i64(0)?;
    let offset = ctx.arg_i64(1)?;
    let data_arg = ctx.arg(2)?;
    if offset < 0 {
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_ERR_TRAP),
            NativeValue::Str("wasm memory write offset must be >= 0".into()),
        ]));
    }
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let write_res = ctx.with_bytes_slice(data_arg, |slice| {
        let mut guard = slot.lock();
        let inst = guard.as_mut().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        Ok(inst.write_memory(offset as usize, slice))
    })?;
    match write_res {
        Ok(()) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
            ]))
        }
    }
}

/// `__wasm_mem_read_str(inst_id: i64, offset: i64, len: i64) -> (i64, String, String)`
pub fn wasm_mem_read_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let inst_id = ctx.arg_i64(0)?;
    let offset = ctx.arg_i64(1)?;
    let len = ctx.arg_i64(2)?;
    if offset < 0 || len < 0 {
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_ERR_TRAP),
            NativeValue::Str("wasm memory read offset/len must be >= 0".into()),
            NativeValue::Str(String::new()),
        ]));
    }
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let read_res: Result<String, (i64, String)> = {
        let guard = slot.lock();
        let inst = guard.as_ref().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        match inst.read_memory(offset as usize, len as usize) {
            Ok(slice) => match std::str::from_utf8(slice) {
                Ok(s) => Ok(s.to_owned()),
                Err(e) => Err((
                    WASM_ERR_VALIDATION,
                    format!("wasm linear memory slice is not valid UTF-8: {e}"),
                )),
            },
            Err(err) => Err(wasm_error_tuple(&err)),
        }
    };
    match read_res {
        Ok(text) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
            NativeValue::Str(text),
        ])),
        Err((code, msg)) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(code),
            NativeValue::Str(msg),
            NativeValue::Str(String::new()),
        ])),
    }
}

/// `__wasm_mem_write_str(inst_id: i64, offset: i64, text: String) -> (i64, String, i64)`
pub fn wasm_mem_write_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let inst_id = ctx.arg_i64(0)?;
    let offset = ctx.arg_i64(1)?;
    let text_arg = ctx.arg(2)?;
    if offset < 0 {
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_ERR_TRAP),
            NativeValue::Str("wasm memory write offset must be >= 0".into()),
            NativeValue::I64(0),
        ]));
    }
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let write_res = ctx.with_str_bytes(text_arg, |slice| {
        let mut guard = slot.lock();
        let inst = guard.as_mut().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        Ok(inst
            .write_memory(offset as usize, slice)
            .map(|()| slice.len() as i64))
    })?;
    match write_res {
        Ok(written) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
            NativeValue::I64(written),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::I64(0),
            ]))
        }
    }
}

/// `__wasm_global_get(inst_id: i64, name: String) -> (i64, String, Any)`
pub fn wasm_global_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let inst_id = ctx.arg_i64(0)?;
    let name = ctx.arg_string(1)?;
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let val_res = {
        let guard = slot.lock();
        let inst = guard.as_ref().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        inst.get_global(&name)
    };
    match val_res {
        Ok(wv) => {
            let nv = ctx.vm.wasm_val_to_native(wv);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(WASM_OK),
                NativeValue::Str(String::new()),
                nv,
            ]))
        }
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::Unit,
            ]))
        }
    }
}

/// `__wasm_global_set(inst_id: i64, name: String, value: Any) -> (i64, String)`
pub fn wasm_global_set(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let inst_id = ctx.arg_i64(0)?;
    let name = ctx.arg_string(1)?;
    let val_arg = ctx.arg(2)?;

    let (module, slot) = {
        let table = ctx.resources();
        let entry = table.get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?;
        (Arc::clone(&entry.module), Arc::clone(&entry.instance))
    };

    let global_ty = module.exports().into_iter().find_map(|exp| {
        if exp.name == name
            && let WasmExportKind::Global { val_type, .. } = exp.kind
        {
            Some(val_type)
        } else {
            None
        }
    });
    let Some(expected_ty) = global_ty else {
        let err = WasmError::UnknownExport(name);
        let (code, msg) = wasm_error_tuple(&err);
        return ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(code),
            NativeValue::Str(msg),
        ]));
    };

    let wv = match ctx.vm.tagged_to_wasm_val(val_arg, expected_ty) {
        Ok(v) => v,
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            return ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
            ]));
        }
    };

    let set_res = {
        let mut guard = slot.lock();
        let inst = guard.as_mut().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        inst.set_global(&name, wv)
    };
    match set_res {
        Ok(()) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
            ]))
        }
    }
}

/// `__wasm_fuel(inst_id: i64) -> ?i64`
pub fn wasm_fuel(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let inst_id = ctx.arg_i64(0)?;
    let slot = {
        let table = ctx.resources();
        Arc::clone(
            &table
                .get::<ManagedWasmInstance>(inst_id, TAG_WASM_INSTANCE)?
                .instance,
        )
    };
    let fuel = {
        let guard = slot.lock();
        let inst = guard.as_ref().ok_or(VmError::InvalidArgument(
            "wasm instance is currently active",
        ))?;
        inst.fuel_remaining().map(|f| f.min(i64::MAX as u64) as i64)
    };
    ctx.return_native(NativeValue::OptI64(fuel))
}

/// `__wasm_run_wasi(bytes: Bytes, args: List<String>, env: Map<String, String>, stdin: Bytes, max_pages: i64, fuel: i64) -> (i64, String, i64, Bytes, Bytes)`
pub fn wasm_run_wasi(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(6)?;
    let bytes_arg = ctx.arg(0)?;
    let args_list = ctx.arg(1)?;
    let env_map = ctx.arg(2)?;
    let stdin_arg = ctx.arg(3)?;
    let max_pages_arg = ctx.arg_i64(4)?;
    let fuel_arg = ctx.arg_i64(5)?;

    let raw_args = ctx.vm.snapshot_list_elements(args_list)?;
    let mut args = Vec::with_capacity(raw_args.len());
    for elem in raw_args {
        let s = heap_string_to_owned(elem).ok_or(VmError::TypeError)?;
        args.push(s);
    }

    let raw_env = {
        let _guard = ctx.vm.stripe_read(env_map)?;
        let layout = ctx.vm.map_layout_unlocked(env_map)?;
        layout.snapshot_entries()?
    };
    let mut env = Vec::with_capacity(raw_env.len());
    for (k, v) in raw_env {
        let val_str = heap_string_to_owned(v).ok_or(VmError::TypeError)?;
        env.push((k, val_str));
    }

    let stdin = ctx.with_bytes_slice(stdin_arg, |s| Ok(s.to_vec()))?;
    let wasm_bytes = ctx.with_bytes_slice(bytes_arg, |s| Ok(s.to_vec()))?;

    let wasi_cfg = WasiConfig {
        args,
        env,
        stdin,
        inherit_stdio: false,
        limits: WasmConfig {
            max_memory_pages: if max_pages_arg <= 0 {
                1024
            } else {
                u32::try_from(max_pages_arg).unwrap_or(u32::MAX)
            },
            max_call_depth: 1024,
            fuel_limit: if fuel_arg < 0 {
                None
            } else {
                Some(fuel_arg as u64)
            },
        },
    };

    let run_res = ctx
        .vm
        .with_native_frame(|_vm| Ok(wasm::run_wasi_bytes(&wasm_bytes, wasi_cfg)))?;

    match run_res {
        Ok(out) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(WASM_OK),
            NativeValue::Str(String::new()),
            NativeValue::I64(i64::from(out.exit_code)),
            NativeValue::Bytes(out.stdout),
            NativeValue::Bytes(out.stderr),
        ])),
        Err(err) => {
            let (code, msg) = wasm_error_tuple(&err);
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(code),
                NativeValue::Str(msg),
                NativeValue::I64(-1),
                NativeValue::Bytes(Vec::new()),
                NativeValue::Bytes(Vec::new()),
            ]))
        }
    }
}

/// `__wasm_close(handle: i64) -> bool`
pub fn wasm_close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let handle = ctx.arg_i64(0)?;
    let closed = ctx
        .resources_mut()
        .close_tags(handle, &[TAG_WASM_MODULE, TAG_WASM_INSTANCE])?;
    if closed {
        ctx.vm.roots().wasm_host_imports.lock().remove(&handle);
    }
    ctx.return_native(NativeValue::Bool(closed))
}
