//! Safe, capability-gated C-ABI Foreign Function Interface (`engine/ffi`).
//!
//! Supports both dynamic shared libraries via `libloading::Library` and pre-registered
//! host C-ABI libraries (`HostLibraryRegistry`). Raw heap pointers are derived strictly
//! inside a non-allocating `Vm::with_native_frame` guard after detaching any shared COW
//! `Bytes` buffers, preventing GC relocation hazards, mutable aliasing, and continuation
//! captures across native C stack frames.

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use interpreter::{BuiltinCtx, BuiltinRoot, VmError};
use runtime::TaggedValue;

/// Resource tag for open FFI library handles in [`interpreter::VmResourceTable`].
pub const TAG_FFI_LIBRARY: u64 = 0x4E45_5353_4646_494C;
/// Resource tag for bound FFI symbol handles in [`interpreter::VmResourceTable`].
pub const TAG_FFI_SYMBOL: u64 = 0x4E45_5353_4646_4953;

const MAX_C_WORDS: usize = 12;

type HostSymbolMap = HashMap<String, usize>;

fn host_registry() -> &'static RwLock<HashMap<String, HostSymbolMap>> {
    static REGISTRY: OnceLock<RwLock<HashMap<String, HostSymbolMap>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register a host-provided C-ABI library by logical name so Nessa packages with
/// `[ffi] allow = true` can bind it via `__ffi_dlopen("<name>")` without requiring external `.so` files.
pub fn register_host_library(name: &str, symbols: &[(&str, *const ())]) {
    let mut map = HashMap::with_capacity(symbols.len());
    for &(sym_name, fn_ptr) in symbols {
        map.insert(sym_name.to_owned(), fn_ptr as usize);
    }
    if let Ok(mut guard) = host_registry().write() {
        guard.insert(name.to_owned(), map);
    }
}

fn lookup_host_library(name: &str) -> Option<HostSymbolMap> {
    host_registry().read().ok()?.get(name).cloned()
}

enum LibraryBacking {
    BuiltinControl,
    Host(HostSymbolMap),
    #[cfg(not(target_arch = "wasm32"))]
    Dynamic(libloading::Library),
}

impl LibraryBacking {
    fn lookup_symbol(&self, symbol: &str) -> Result<Option<usize>, VmError> {
        if symbol.is_empty() || symbol.contains('\0') {
            return Err(VmError::InvalidArgument("invalid FFI symbol name"));
        }
        match self {
            Self::BuiltinControl => Ok(None),
            Self::Host(map) => Ok(map.get(symbol).copied()),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Dynamic(lib) => {
                let mut nul_terminated = Vec::with_capacity(symbol.len() + 1);
                nul_terminated.extend_from_slice(symbol.as_bytes());
                nul_terminated.push(0);
                // SAFETY: `nul_terminated` is NUL-terminated; we extract the raw code address
                // while every `FfiSymbol` and `WeakFinalizer` retains `Arc<LibraryBacking>`
                // so `dlclose` cannot occur while the symbol pointer remains callable.
                unsafe {
                    let Ok(sym) = lib.get::<*const ()>(&nul_terminated) else {
                        return Ok(None);
                    };
                    Ok(Some((*sym) as usize))
                }
            }
        }
    }
}

struct FfiLibraryHandle {
    _name: String,
    backing: Arc<LibraryBacking>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CParamKind {
    I64,
    U64,
    Usize,
    Isize,
    I32,
    U32,
    Bool,
    PtrRo,
    PtrMut,
    SliceRo,
    BufMut,
}

impl CParamKind {
    fn c_word_count(self) -> usize {
        match self {
            Self::SliceRo | Self::BufMut => 2,
            _ => 1,
        }
    }

    fn is_mutable_bytes(self) -> bool {
        matches!(self, Self::PtrMut | Self::BufMut)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CReturnKind {
    Void,
    I64,
    U64,
    Usize,
    Isize,
    I32,
    U32,
    Bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SymbolKind {
    /// Builtin handle-close operation (`__ffi_dlsym(0, "close", "i64->bool")`).
    CloseHandle,
    /// Attaches a GC weak finalizer calling `extern "C" fn(i64)` when an object is swept.
    GcCleanupI64,
    /// Standard C ABI function call.
    CFunction {
        params: Vec<CParamKind>,
        total_c_words: usize,
        ret: CReturnKind,
    },
}

struct FfiSymbolHandle {
    backing: Arc<LibraryBacking>,
    fn_addr: usize,
    kind: SymbolKind,
}

fn parse_param_kind(tok: &str) -> Result<CParamKind, VmError> {
    match tok.trim() {
        "i64" => Ok(CParamKind::I64),
        "u64" => Ok(CParamKind::U64),
        "usize" => Ok(CParamKind::Usize),
        "isize" => Ok(CParamKind::Isize),
        "i32" => Ok(CParamKind::I32),
        "u32" => Ok(CParamKind::U32),
        "bool" => Ok(CParamKind::Bool),
        "ptr_ro" => Ok(CParamKind::PtrRo),
        "ptr_mut" => Ok(CParamKind::PtrMut),
        "slice_ro" => Ok(CParamKind::SliceRo),
        "buf_mut" => Ok(CParamKind::BufMut),
        _ => Err(VmError::InvalidArgument(
            "invalid FFI parameter type in signature descriptor",
        )),
    }
}

fn parse_return_kind(tok: &str) -> Result<CReturnKind, VmError> {
    match tok.trim() {
        "void" | "unit" | "()" => Ok(CReturnKind::Void),
        "i64" => Ok(CReturnKind::I64),
        "u64" => Ok(CReturnKind::U64),
        "usize" => Ok(CReturnKind::Usize),
        "isize" => Ok(CReturnKind::Isize),
        "i32" => Ok(CReturnKind::I32),
        "u32" => Ok(CReturnKind::U32),
        "bool" => Ok(CReturnKind::Bool),
        _ => Err(VmError::InvalidArgument(
            "invalid FFI return type in signature descriptor",
        )),
    }
}

fn parse_signature(sig: &str) -> Result<SymbolKind, VmError> {
    let trimmed = sig.trim();
    if trimmed == "on_gc:any,i64->void" {
        return Ok(SymbolKind::GcCleanupI64);
    }
    let Some((lhs, rhs)) = trimmed.split_once("->") else {
        return Err(VmError::InvalidArgument(
            "FFI signature descriptor must contain '->'",
        ));
    };
    let lhs = lhs.trim();
    let mut params = Vec::new();
    let mut total_c_words = 0usize;
    if !lhs.is_empty() && lhs != "void" && lhs != "()" {
        for part in lhs.split(',') {
            let kind = parse_param_kind(part)?;
            total_c_words += kind.c_word_count();
            params.push(kind);
        }
    }
    if total_c_words > MAX_C_WORDS {
        return Err(VmError::InvalidArgument(
            "FFI signature exceeds maximum supported C word count (12)",
        ));
    }
    let ret = parse_return_kind(rhs)?;
    Ok(SymbolKind::CFunction {
        params,
        total_c_words,
        ret,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn open_dynamic_library(spec: &str) -> Result<libloading::Library, VmError> {
    // SAFETY: `dlopen` loads a host dynamic library requested by a capability-authorized
    // `[ffi]` package; any C constructor runs outside managed GC frames.
    if let Ok(lib) = unsafe { libloading::Library::new(spec) } {
        return Ok(lib);
    }
    let has_sep = spec.contains('/') || spec.contains('\\');
    let path = Path::new(spec);
    if !has_sep && path.extension().is_none() {
        if cfg!(target_os = "macos") {
            let candidate = format!("lib{spec}.dylib");
            // SAFETY: Capability-authorized dynamic library load outside managed GC frames.
            if let Ok(lib) = unsafe { libloading::Library::new(&candidate) } {
                return Ok(lib);
            }
        } else if cfg!(target_os = "windows") {
            for candidate in [format!("{spec}.dll"), format!("lib{spec}.dll")] {
                // SAFETY: Capability-authorized dynamic library load outside managed GC frames.
                if let Ok(lib) = unsafe { libloading::Library::new(&candidate) } {
                    return Ok(lib);
                }
            }
        } else {
            let candidate = format!("lib{spec}.so");
            // SAFETY: Capability-authorized dynamic library load outside managed GC frames.
            if let Ok(lib) = unsafe { libloading::Library::new(&candidate) } {
                return Ok(lib);
            }
        }
    }
    Err(VmError::InvalidArgument(
        "failed to load FFI shared library",
    ))
}

/// Builtin implementation for `__ffi_dlopen(path_or_name: String) -> i64`.
pub fn ffi_dlopen(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let name = ctx.arg_string(0)?;
    if name.is_empty() || name.contains('\0') {
        return Err(VmError::InvalidArgument("invalid FFI library name"));
    }
    if !ctx.is_ffi_library_allowed(&name) {
        return Err(VmError::InvalidArgument(
            "FFI library is not permitted by the VM allowlist",
        ));
    }
    let backing = if let Some(host_map) = lookup_host_library(&name) {
        Arc::new(LibraryBacking::Host(host_map))
    } else {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Ok(lib) = open_dynamic_library(&name) else {
                return ctx.return_i64(-1);
            };
            Arc::new(LibraryBacking::Dynamic(lib))
        }
        #[cfg(target_arch = "wasm32")]
        {
            return ctx.return_i64(-1);
        }
    };
    let handle = ctx.resources_mut().insert(
        TAG_FFI_LIBRARY,
        FfiLibraryHandle {
            _name: name,
            backing,
        },
    )?;
    ctx.return_i64(handle)
}

/// Builtin implementation for `__ffi_dlsym(lib_handle: i64, symbol: String, sig: String) -> i64`.
///
/// Passing `lib_handle == 0` with `symbol == "close"` and `sig == "i64->bool"` binds
/// a handle-closing symbol that deterministically closes any open library or symbol handle.
pub fn ffi_dlsym(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let lib_id = ctx.arg_i64(0)?;
    let sym_name = ctx.arg_string(1)?;
    let sig_str = ctx.arg_string(2)?;

    if lib_id == 0 {
        if sym_name == "close" && sig_str.trim() == "i64->bool" {
            let sym_id = ctx.resources_mut().insert(
                TAG_FFI_SYMBOL,
                FfiSymbolHandle {
                    backing: Arc::new(LibraryBacking::BuiltinControl),
                    fn_addr: 0,
                    kind: SymbolKind::CloseHandle,
                },
            )?;
            return ctx.return_i64(sym_id);
        }
        return Err(VmError::InvalidArgument("invalid FFI library handle"));
    }

    let kind = parse_signature(&sig_str)?;
    let backing = {
        let resources = ctx.resources();
        let lib = resources.get::<FfiLibraryHandle>(lib_id, TAG_FFI_LIBRARY)?;
        Arc::clone(&lib.backing)
    };
    let Some(fn_addr) = backing.lookup_symbol(&sym_name)? else {
        return ctx.return_i64(-1);
    };
    if fn_addr == 0 {
        return Err(VmError::InvalidArgument(
            "FFI symbol resolved to null pointer",
        ));
    }
    let sym_id = ctx.resources_mut().insert(
        TAG_FFI_SYMBOL,
        FfiSymbolHandle {
            backing,
            fn_addr,
            kind,
        },
    )?;
    ctx.return_i64(sym_id)
}

/// Builtin implementation for `__ffi_call(sym_handle: i64, args: List) -> Any`.
pub fn ffi_call(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let sym_id = ctx.arg_i64(0)?;
    let list_root = ctx.arg_rooted(1)?;
    let list_val = ctx.load_rooted(&list_root)?;
    let len = ctx.list_len(list_val)?;
    let mut roots = Vec::with_capacity(len);
    for idx in 0..len {
        roots.push(ctx.list_get_rooted(&list_root, idx)?);
    }
    invoke_symbol_with_roots(ctx, sym_id, &roots)
}

/// Builtin implementation for `__ffi_call4(sym_handle: i64, a0..a3) -> Any` (up to 4 Nessa args).
pub fn ffi_call4(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let argc = ctx.arg_count();
    if !(1..=5).contains(&argc) {
        return Err(VmError::ArityError {
            expected: 5,
            got: argc,
        });
    }
    let sym_id = ctx.arg_i64(0)?;
    let mut roots = Vec::with_capacity((argc - 1) as usize);
    for i in 1..argc {
        roots.push(ctx.arg_rooted(i)?);
    }
    invoke_symbol_with_roots(ctx, sym_id, &roots)
}

/// Builtin implementation for `__ffi_call6(sym_handle: i64, a0..a5) -> Any` (up to 6 Nessa args).
pub fn ffi_call6(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let argc = ctx.arg_count();
    if !(1..=7).contains(&argc) {
        return Err(VmError::ArityError {
            expected: 7,
            got: argc,
        });
    }
    let sym_id = ctx.arg_i64(0)?;
    let mut roots = Vec::with_capacity((argc - 1) as usize);
    for i in 1..argc {
        roots.push(ctx.arg_rooted(i)?);
    }
    invoke_symbol_with_roots(ctx, sym_id, &roots)
}

fn checked_scalar_word(
    ctx: &BuiltinCtx<'_>,
    val: TaggedValue,
    kind: CParamKind,
) -> Result<usize, VmError> {
    match kind {
        CParamKind::I64 => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            Ok(n as usize)
        }
        CParamKind::U64 => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::NumericOverflow)?;
            Ok(n as usize)
        }
        CParamKind::Usize => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::InvalidIndex)?;
            usize::try_from(n).map_err(|_| VmError::InvalidIndex)
        }
        CParamKind::Isize => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let isz = isize::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(isz as usize)
        }
        CParamKind::I32 => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = i32::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok((v as isize) as usize)
        }
        CParamKind::U32 => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = u32::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(v as usize)
        }
        CParamKind::Bool => Ok(usize::from(val.as_bool().ok_or(VmError::TypeError)?)),
        _ => unreachable!("slice/pointer kinds handled in native frame"),
    }
}

fn invoke_symbol_with_roots(
    ctx: &mut BuiltinCtx<'_>,
    sym_id: i64,
    roots: &[BuiltinRoot],
) -> Result<(), VmError> {
    let (backing, fn_addr, kind) = {
        let resources = ctx.resources();
        let sym = resources.get::<FfiSymbolHandle>(sym_id, TAG_FFI_SYMBOL)?;
        (Arc::clone(&sym.backing), sym.fn_addr, sym.kind.clone())
    };

    match kind {
        SymbolKind::CloseHandle => {
            if roots.len() != 1 {
                return Err(VmError::ArityError {
                    expected: 1,
                    got: roots.len() as u8,
                });
            }
            let raw_val = ctx.load_rooted(&roots[0])?;
            let target_id = ctx
                .number_of(raw_val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let closed = ctx
                .resources_mut()
                .close_tags(target_id, &[TAG_FFI_LIBRARY, TAG_FFI_SYMBOL])?;
            ctx.return_bool(closed);
            Ok(())
        }
        SymbolKind::GcCleanupI64 => {
            if roots.len() != 2 {
                return Err(VmError::ArityError {
                    expected: 2,
                    got: roots.len() as u8,
                });
            }
            let owner_val = ctx.load_rooted(&roots[0])?;
            let handle_val = ctx.load_rooted(&roots[1])?;
            let raw_handle = ctx
                .number_of(handle_val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            ctx.attach_weak_finalizer(owner_val, move || {
                let _keep_alive = &backing;
                // SAFETY: `fn_addr` was bound with `"on_gc:any,i64->void"` (`extern "C" fn(i64)`),
                // and `_keep_alive` retains the shared library across finalizer execution.
                unsafe {
                    let cleanup_fn: unsafe extern "C" fn(i64) = std::mem::transmute(fn_addr);
                    cleanup_fn(raw_handle);
                }
            })?;
            ctx.return_unit();
            Ok(())
        }
        SymbolKind::CFunction {
            params,
            total_c_words,
            ret,
        } => {
            if roots.len() != params.len() {
                return Err(VmError::ArityError {
                    expected: params.len() as u8,
                    got: roots.len() as u8,
                });
            }

            // Phase 1: Detach any shared COW `Bytes` buffers BEFORE taking raw pointers.
            for (root, &param) in roots.iter().zip(&params) {
                if param.is_mutable_bytes() {
                    ctx.prepare_ffi_bytes_mut(root)?;
                }
            }

            // Pre-validate scalars and snapshot reloaded `TaggedValue` handles after all COW allocations.
            let mut reloaded = Vec::with_capacity(roots.len());
            let mut predecoded_scalars = vec![0usize; roots.len()];
            for (idx, (root, &param)) in roots.iter().zip(&params).enumerate() {
                let val = ctx.load_rooted(root)?;
                if !matches!(
                    param,
                    CParamKind::PtrRo
                        | CParamKind::PtrMut
                        | CParamKind::SliceRo
                        | CParamKind::BufMut
                ) {
                    predecoded_scalars[idx] = checked_scalar_word(ctx, val, param)?;
                }
                reloaded.push(val);
            }

            // Phase 2: Non-allocating native frame guard (`&Vm` immutable, `native_frame_depth > 0`).
            let _keep_alive = backing;
            let raw_ret = ctx.with_native_ffi_frame(|vm| {
                let mut c_words = [0usize; MAX_C_WORDS];
                let mut cursor = 0usize;
                let mut mut_identities: Vec<*const u8> = Vec::new();
                let mut mut_ranges: Vec<(usize, usize)> = Vec::new();
                let mut ro_ranges: Vec<(usize, usize)> = Vec::new();

                for (idx, (&val, &param)) in reloaded.iter().zip(&params).enumerate() {
                    match param {
                        CParamKind::PtrRo => {
                            let (ptr, len) = vm.raw_ro_slice(val)?;
                            if len > 0 {
                                ro_ranges.push((ptr as usize, (ptr as usize).saturating_add(len)));
                            }
                            c_words[cursor] = ptr as usize;
                            cursor += 1;
                        }
                        CParamKind::SliceRo => {
                            let (ptr, len) = vm.raw_ro_slice(val)?;
                            if len > 0 {
                                ro_ranges.push((ptr as usize, (ptr as usize).saturating_add(len)));
                            }
                            c_words[cursor] = ptr as usize;
                            c_words[cursor + 1] = len;
                            cursor += 2;
                        }
                        CParamKind::PtrMut => {
                            let (ptr, len, buf_id) = vm.raw_mut_bytes_buf(val)?;
                            if !buf_id.is_null() {
                                if mut_identities.contains(&buf_id) {
                                    return Err(VmError::InvalidArgument(
                                        "aliasing mutable Bytes buffers in FFI call",
                                    ));
                                }
                                mut_identities.push(buf_id);
                            }
                            if len > 0 {
                                mut_ranges.push((ptr as usize, (ptr as usize).saturating_add(len)));
                            }
                            c_words[cursor] = ptr as usize;
                            cursor += 1;
                        }
                        CParamKind::BufMut => {
                            let (ptr, len, buf_id) = vm.raw_mut_bytes_buf(val)?;
                            if !buf_id.is_null() {
                                if mut_identities.contains(&buf_id) {
                                    return Err(VmError::InvalidArgument(
                                        "aliasing mutable Bytes buffers in FFI call",
                                    ));
                                }
                                mut_identities.push(buf_id);
                            }
                            if len > 0 {
                                mut_ranges.push((ptr as usize, (ptr as usize).saturating_add(len)));
                            }
                            c_words[cursor] = ptr as usize;
                            c_words[cursor + 1] = len;
                            cursor += 2;
                        }
                        _ => {
                            c_words[cursor] = predecoded_scalars[idx];
                            cursor += 1;
                        }
                    }
                }
                for &(ms, me) in &mut_ranges {
                    for &(rs, re) in &ro_ranges {
                        if ms < re && rs < me {
                            return Err(VmError::InvalidArgument(
                                "aliasing read-only and mutable Bytes slices in FFI call",
                            ));
                        }
                    }
                }
                debug_assert_eq!(cursor, total_c_words);

                // SAFETY: `fn_addr` belongs to a live library (`_keep_alive`), `c_words` holds
                // validated C-ABI scalar/pointer words whose backing objects are rooted and
                // unmoved (no managed allocation can occur inside `with_native_ffi_frame`),
                // and mutable `Bytes` buffers are uniquely owned and non-aliasing.
                let result = unsafe { dispatch_extern_c(fn_addr, total_c_words, &c_words, ret) };
                Ok(result)
            })?;

            // Phase 3: Marshal C return value back into Nessa `TaggedValue`.
            match ret {
                CReturnKind::Void => {
                    ctx.return_unit();
                    Ok(())
                }
                CReturnKind::I64 => ctx.return_i64(raw_ret as i64),
                CReturnKind::U64 | CReturnKind::Usize => ctx.return_u64(raw_ret as u64),
                CReturnKind::Isize => ctx.return_i64((raw_ret as isize) as i64),
                CReturnKind::I32 => ctx.return_i64(i64::from(raw_ret as i32)),
                CReturnKind::U32 => ctx.return_u64(u64::from(raw_ret as u32)),
                CReturnKind::Bool => {
                    ctx.return_bool((raw_ret & 0xFF) != 0);
                    Ok(())
                }
            }
        }
    }
}

unsafe fn dispatch_extern_c(
    fn_addr: usize,
    arity: usize,
    w: &[usize; MAX_C_WORDS],
    ret: CReturnKind,
) -> usize {
    if ret == CReturnKind::Void {
        // SAFETY: Void-returning C functions must not have an undefined return register read.
        unsafe {
            match arity {
                0 => (std::mem::transmute::<usize, unsafe extern "C" fn()>(fn_addr))(),
                1 => (std::mem::transmute::<usize, unsafe extern "C" fn(usize)>(fn_addr))(w[0]),
                2 => (std::mem::transmute::<usize, unsafe extern "C" fn(usize, usize)>(fn_addr))(
                    w[0], w[1],
                ),
                3 => (std::mem::transmute::<usize, unsafe extern "C" fn(usize, usize, usize)>(
                    fn_addr,
                ))(w[0], w[1], w[2]),
                4 => {
                    (std::mem::transmute::<usize, unsafe extern "C" fn(usize, usize, usize, usize)>(
                        fn_addr,
                    ))(w[0], w[1], w[2], w[3])
                }
                5 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize, usize),
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4]),
                6 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize, usize, usize),
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4], w[5]),
                7 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize, usize, usize, usize),
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4], w[5], w[6]),
                8 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize, usize, usize, usize, usize),
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]),
                9 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ),
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8]
                ),
                10 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ),
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9]
                ),
                11 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ),
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10],
                ),
                12 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ),
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11],
                ),
                _ => unreachable!("checked <= MAX_C_WORDS"),
            }
        }
        0
    } else {
        // SAFETY: Integer/pointer-returning C functions return in the primary word register (`rax`/`x0`).
        unsafe {
            match arity {
                0 => (std::mem::transmute::<usize, unsafe extern "C" fn() -> usize>(fn_addr))(),
                1 => (std::mem::transmute::<usize, unsafe extern "C" fn(usize) -> usize>(fn_addr))(
                    w[0],
                ),
                2 => (std::mem::transmute::<usize, unsafe extern "C" fn(usize, usize) -> usize>(
                    fn_addr,
                ))(w[0], w[1]),
                3 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize) -> usize,
                >(fn_addr))(w[0], w[1], w[2]),
                4 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize) -> usize,
                >(fn_addr))(w[0], w[1], w[2], w[3]),
                5 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize, usize) -> usize,
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4]),
                6 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize, usize, usize) -> usize,
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4], w[5]),
                7 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(usize, usize, usize, usize, usize, usize, usize) -> usize,
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4], w[5], w[6]),
                8 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> usize,
                >(fn_addr))(w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]),
                9 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> usize,
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8]
                ),
                10 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> usize,
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9]
                ),
                11 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> usize,
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10],
                ),
                12 => (std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> usize,
                >(fn_addr))(
                    w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11],
                ),
                _ => unreachable!("checked <= MAX_C_WORDS"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_parser_handles_slices_scalars_void_and_gc_hook() {
        assert_eq!(
            parse_signature("on_gc:any,i64->void").unwrap(),
            SymbolKind::GcCleanupI64
        );
        assert_eq!(
            parse_signature("slice_ro,u32,usize,usize,buf_mut->i64").unwrap(),
            SymbolKind::CFunction {
                params: vec![
                    CParamKind::SliceRo,
                    CParamKind::U32,
                    CParamKind::Usize,
                    CParamKind::Usize,
                    CParamKind::BufMut,
                ],
                total_c_words: 7,
                ret: CReturnKind::I64,
            }
        );
        assert_eq!(
            parse_signature("->void").unwrap(),
            SymbolKind::CFunction {
                params: vec![],
                total_c_words: 0,
                ret: CReturnKind::Void,
            }
        );
        assert!(parse_signature("unknown->i64").is_err());
        assert!(parse_signature("i64").is_err());
    }
}
