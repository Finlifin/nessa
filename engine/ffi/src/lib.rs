//! Safe, capability-gated C-ABI Foreign Function Interface (`engine/ffi`).
//!
//! Supports both dynamic shared libraries via `libloading::Library` and pre-registered
//! host C-ABI libraries (`HostLibraryRegistry`). Raw heap pointers are derived strictly
//! inside a non-allocating `Vm::with_native_frame` guard after detaching any shared COW
//! `Bytes` buffers, preventing GC relocation hazards, mutable aliasing, and continuation
//! captures across native C stack frames.

use std::collections::HashMap;
use std::ffi::c_char;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use interpreter::{BuiltinCtx, BuiltinRoot, VmError};
use runtime::{Number, TaggedValue};

/// Resource tag for open FFI library handles in [`interpreter::VmResourceTable`].
pub const TAG_FFI_LIBRARY: u64 = 0x4E45_5353_4646_494C;
/// Resource tag for bound FFI symbol handles in [`interpreter::VmResourceTable`].
pub const TAG_FFI_SYMBOL: u64 = 0x4E45_5353_4646_4953;

const MAX_C_WORDS: usize = 12;
const MAX_FFI_CSTR_BYTES: usize = 64 * 1024 * 1024;

type HostSymbolMap = HashMap<String, usize>;

extern "C" fn test_add_i32(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

extern "C" fn test_narrow_mix(a: i8, b: u8, c: i16, d: u16) -> i32 {
    i32::from(a) + i32::from(b) + i32::from(c) + i32::from(d)
}

extern "C" fn test_f32_math(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

extern "C" fn test_f64_mix(tag: i32, scale: f64, bias: f32, count: i64) -> f64 {
    (tag as f64) * scale + (bias as f64) + (count as f64)
}

unsafe extern "C" fn test_cstr_len(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    let mut len = 0usize;
    // SAFETY: `s` is a non-null NUL-terminated C string supplied by the FFI caller;
    // byte-by-byte reads have alignment 1 (`c_char`), no mutable aliasing, and stop
    // at the first NUL byte.
    unsafe {
        while *s.add(len) != 0 {
            len += 1;
        }
    }
    len
}

extern "C" fn test_cstr_greeting() -> *const c_char {
    c"hello from c".as_ptr()
}

extern "C" fn test_ptr_offset(p: *const u8, off: isize) -> *const u8 {
    p.wrapping_offset(off)
}

#[allow(clippy::too_many_arguments)]
extern "C" fn test_many_args(
    a1: i64,
    a2: f64,
    a3: i32,
    a4: f32,
    a5: i64,
    a6: f64,
    a7: i32,
    a8: i64,
) -> f64 {
    (a1 as f64) + a2 + (a3 as f64) + (a4 as f64) + (a5 as f64) + a6 + (a7 as f64) + (a8 as f64)
}

fn builtin_test_host_symbols() -> HostSymbolMap {
    let entries: &[(&str, *const ())] = &[
        ("test_add_i32", test_add_i32 as *const ()),
        ("test_narrow_mix", test_narrow_mix as *const ()),
        ("test_f32_math", test_f32_math as *const ()),
        ("test_f64_mix", test_f64_mix as *const ()),
        ("test_cstr_len", test_cstr_len as *const ()),
        ("test_cstr_greeting", test_cstr_greeting as *const ()),
        ("test_ptr_offset", test_ptr_offset as *const ()),
        ("test_many_args", test_many_args as *const ()),
    ];
    let mut map = HashMap::with_capacity(entries.len());
    for &(name, ptr) in entries {
        map.insert(name.to_owned(), ptr as usize);
    }
    map
}

fn host_registry() -> &'static RwLock<HashMap<String, HostSymbolMap>> {
    static REGISTRY: OnceLock<RwLock<HashMap<String, HostSymbolMap>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut init = HashMap::new();
        init.insert("__nessa_ffi_test".to_owned(), builtin_test_host_symbols());
        RwLock::new(init)
    })
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

/// Foreign calling convention selector on an FFI signature (`"c:"` or `"system:"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallConv {
    C,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CParamKind {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    Isize,
    Usize,
    F32,
    F64,
    Bool,
    Ptr,
    CStr,
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
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    Isize,
    Usize,
    F32,
    F64,
    Bool,
    Ptr,
    CStr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SymbolKind {
    /// Builtin handle-close operation (`__ffi_dlsym(0, "close", "i64->bool")`).
    CloseHandle,
    /// Attaches a GC weak finalizer calling `extern "C" fn(i64)` when an object is swept.
    GcCleanupI64,
    /// Standard C/System ABI function call.
    CFunction {
        callconv: CallConv,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CArgWord {
    Gp(usize),
    F32(u32),
    F64(u64),
}

#[derive(Debug, Clone, Copy)]
enum PredecodedArg {
    Word(CArgWord),
    CStr(usize),
    DeferredSlice,
}

#[derive(Debug, Clone, Copy)]
struct RawCReturn {
    gp: u64,
    fp: u64,
}

fn normalize_type_token(tok: &str) -> &str {
    let trimmed = tok.trim();
    trimmed.rsplit('.').next().unwrap_or(trimmed).trim()
}

fn parse_param_kind(tok: &str) -> Result<CParamKind, VmError> {
    match normalize_type_token(tok) {
        "i8" => Ok(CParamKind::I8),
        "u8" => Ok(CParamKind::U8),
        "i16" => Ok(CParamKind::I16),
        "u16" => Ok(CParamKind::U16),
        "i32" => Ok(CParamKind::I32),
        "u32" => Ok(CParamKind::U32),
        "i64" => Ok(CParamKind::I64),
        "u64" => Ok(CParamKind::U64),
        "isize" => Ok(CParamKind::Isize),
        "usize" => Ok(CParamKind::Usize),
        "f32" => Ok(CParamKind::F32),
        "f64" => Ok(CParamKind::F64),
        "bool" => Ok(CParamKind::Bool),
        "Ptr" | "ptr" => Ok(CParamKind::Ptr),
        "CStr" | "cstr" => Ok(CParamKind::CStr),
        "PtrRo" | "ptr_ro" => Ok(CParamKind::PtrRo),
        "PtrMut" | "ptr_mut" => Ok(CParamKind::PtrMut),
        "SliceRo" | "slice_ro" => Ok(CParamKind::SliceRo),
        "BufMut" | "buf_mut" => Ok(CParamKind::BufMut),
        _ => Err(VmError::InvalidArgument(
            "invalid FFI parameter type in signature descriptor",
        )),
    }
}

fn parse_return_kind(tok: &str) -> Result<CReturnKind, VmError> {
    if tok.trim() == "()" {
        return Ok(CReturnKind::Void);
    }
    match normalize_type_token(tok) {
        "Unit" | "unit" | "void" => Ok(CReturnKind::Void),
        "i8" => Ok(CReturnKind::I8),
        "u8" => Ok(CReturnKind::U8),
        "i16" => Ok(CReturnKind::I16),
        "u16" => Ok(CReturnKind::U16),
        "i32" => Ok(CReturnKind::I32),
        "u32" => Ok(CReturnKind::U32),
        "i64" => Ok(CReturnKind::I64),
        "u64" => Ok(CReturnKind::U64),
        "isize" => Ok(CReturnKind::Isize),
        "usize" => Ok(CReturnKind::Usize),
        "f32" => Ok(CReturnKind::F32),
        "f64" => Ok(CReturnKind::F64),
        "bool" => Ok(CReturnKind::Bool),
        "Ptr" | "ptr" => Ok(CReturnKind::Ptr),
        "CStr" | "cstr" => Ok(CReturnKind::CStr),
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

    let (callconv, body) = if let Some((prefix, rest)) = trimmed.split_once(':') {
        let conv = match prefix.trim().to_ascii_lowercase().as_str() {
            "c" => CallConv::C,
            "system" => CallConv::System,
            _ => {
                return Err(VmError::InvalidArgument(
                    "unsupported FFI calling convention in signature descriptor",
                ));
            }
        };
        (conv, rest.trim())
    } else {
        (CallConv::C, trimmed)
    };

    let (lhs, rhs) = if let Some(after_fn) = body.strip_prefix("fn") {
        let after_fn = after_fn.trim_start();
        let Some(after_open) = after_fn.strip_prefix('(') else {
            return Err(VmError::InvalidArgument(
                "invalid FFI fn(...) signature descriptor",
            ));
        };
        let Some((params_inner, after_close)) = after_open.split_once(')') else {
            return Err(VmError::InvalidArgument(
                "unclosed '(' in FFI fn(...) signature descriptor",
            ));
        };
        let after_close = after_close.trim();
        let ret_part = if after_close.is_empty() {
            "Unit"
        } else if let Some(ret_str) = after_close.strip_prefix("->") {
            ret_str.trim()
        } else {
            return Err(VmError::InvalidArgument(
                "expected '->' after ')' in FFI fn(...) signature descriptor",
            ));
        };
        (params_inner.trim(), ret_part)
    } else {
        let Some((l, r)) = body.split_once("->") else {
            return Err(VmError::InvalidArgument(
                "FFI signature descriptor must contain '->'",
            ));
        };
        (l.trim(), r.trim())
    };

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
        callconv,
        params,
        total_c_words,
        ret,
    })
}

fn read_bounded_cstr(addr: usize, max_len: usize) -> Result<String, VmError> {
    let ptr = addr as *const u8;
    let mut nul_pos = None;
    for idx in 0..max_len {
        // SAFETY: `addr` is a non-null C string address supplied or returned across a
        // capability-authorized `[ffi]` boundary. Scanning one byte (`u8`, alignment 1,
        // no mutable aliasing) stops immediately at the first NUL byte without reading
        // past `\0` into potentially unmapped pages when `max_len` is an upper bound.
        let byte = unsafe { *ptr.add(idx) };
        if byte == 0 {
            nul_pos = Some(idx);
            break;
        }
    }
    let Some(len) = nul_pos else {
        return Err(VmError::InvalidArgument(
            "FFI C string exceeds max_len without NUL terminator",
        ));
    };
    let slice = if len == 0 {
        &[][..]
    } else {
        // SAFETY: `ptr..ptr.add(len)` was verified readable byte-by-byte up to the NUL
        // terminator with alignment 1 (`u8`) and no mutable Rust references aliasing it.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    };
    let s = std::str::from_utf8(slice)
        .map_err(|_| VmError::InvalidArgument("FFI C string is not valid UTF-8"))?;
    Ok(s.to_owned())
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
        let trimmed = sig_str.trim();
        if sym_name == "close"
            && (trimmed == "i64->bool"
                || trimmed == "fn(i64) -> bool"
                || trimmed == "c:fn(i64) -> bool")
        {
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

/// Builtin implementation for `__ffi_read_cstr(addr: usize, max_len: usize) -> ?String`.
pub fn ffi_read_cstr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let addr_val = ctx.arg(0)?;
    let addr = ctx.extract_ptr_addr(addr_val)?;
    let max_val = ctx.arg(1)?;
    let peeled_max = ctx.peel_newtype(max_val)?;
    let max_num = ctx.number_of(peeled_max)?;
    if matches!(max_num, Number::F64(_)) {
        return Err(VmError::TypeError);
    }
    let max_u64 = max_num.to_u64_checked().ok_or(VmError::NumericOverflow)?;
    let max_len = usize::try_from(max_u64).map_err(|_| VmError::NumericOverflow)?;
    if addr == 0 {
        ctx.return_null();
        return Ok(());
    }
    let Ok(s) = read_bounded_cstr(addr, max_len) else {
        ctx.return_null();
        return Ok(());
    };
    ctx.check_string_size(s.len())?;
    ctx.return_string(&s)
}

fn predecode_arg(
    ctx: &BuiltinCtx<'_>,
    val: TaggedValue,
    kind: CParamKind,
    cstr_buffers: &mut Vec<Vec<u8>>,
) -> Result<PredecodedArg, VmError> {
    match kind {
        CParamKind::I8 => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = i8::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp((v as isize) as usize)))
        }
        CParamKind::U8 => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = u8::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(v as usize)))
        }
        CParamKind::I16 => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = i16::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp((v as isize) as usize)))
        }
        CParamKind::U16 => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = u16::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(v as usize)))
        }
        CParamKind::I32 => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = i32::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp((v as isize) as usize)))
        }
        CParamKind::U32 => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let v = u32::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(v as usize)))
        }
        CParamKind::I64 => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(n as usize)))
        }
        CParamKind::U64 => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(n as usize)))
        }
        CParamKind::Isize => {
            let n = ctx
                .number_of(val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            let isz = isize::try_from(n).map_err(|_| VmError::NumericOverflow)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(isz as usize)))
        }
        CParamKind::Usize => {
            let n = ctx
                .number_of(val)?
                .to_u64_checked()
                .ok_or(VmError::InvalidIndex)?;
            let usz = usize::try_from(n).map_err(|_| VmError::InvalidIndex)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(usz)))
        }
        CParamKind::F32 => {
            let f = ctx.number_of(val)?.to_f64() as f32;
            Ok(PredecodedArg::Word(CArgWord::F32(f.to_bits())))
        }
        CParamKind::F64 => {
            let f = ctx.number_of(val)?.to_f64();
            Ok(PredecodedArg::Word(CArgWord::F64(f.to_bits())))
        }
        CParamKind::Bool => {
            let b = val.as_bool().ok_or(VmError::TypeError)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(usize::from(b))))
        }
        CParamKind::Ptr => {
            let addr = ctx.extract_ptr_addr(val)?;
            Ok(PredecodedArg::Word(CArgWord::Gp(addr)))
        }
        CParamKind::CStr => {
            let buf = ctx.with_str_bytes(val, |bytes| {
                if bytes.contains(&0) {
                    return Err(VmError::InvalidArgument(
                        "FFI CStr contains interior NUL byte",
                    ));
                }
                let mut nul_buf = Vec::with_capacity(bytes.len() + 1);
                nul_buf.extend_from_slice(bytes);
                nul_buf.push(0);
                Ok(nul_buf)
            })?;
            let idx = cstr_buffers.len();
            cstr_buffers.push(buf);
            Ok(PredecodedArg::CStr(idx))
        }
        CParamKind::PtrRo | CParamKind::PtrMut | CParamKind::SliceRo | CParamKind::BufMut => {
            Ok(PredecodedArg::DeferredSlice)
        }
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
                    got: u8::try_from(roots.len()).unwrap_or(u8::MAX),
                });
            }
            let raw_val = ctx.load_rooted(&roots[0])?;
            let peeled = ctx.peel_newtype(raw_val)?;
            let target_id = ctx
                .number_of(peeled)?
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
                    got: u8::try_from(roots.len()).unwrap_or(u8::MAX),
                });
            }
            let owner_val = ctx.load_rooted(&roots[0])?;
            let handle_val = ctx.peel_newtype(ctx.load_rooted(&roots[1])?)?;
            let raw_handle = ctx
                .number_of(handle_val)?
                .to_i64_checked()
                .ok_or(VmError::NumericOverflow)?;
            ctx.attach_weak_finalizer(owner_val, move || {
                let _keep_alive = &backing;
                // SAFETY: `fn_addr` was bound with `"on_gc:any,i64->void"` (`extern "C" fn(i64)`),
                // and `_keep_alive` retains the shared library across finalizer execution.
                unsafe {
                    let cleanup_fn =
                        std::mem::transmute::<usize, unsafe extern "C" fn(i64)>(fn_addr);
                    cleanup_fn(raw_handle);
                }
            })?;
            ctx.return_unit();
            Ok(())
        }
        SymbolKind::CFunction {
            callconv,
            params,
            total_c_words,
            ret,
        } => {
            if roots.len() != params.len() {
                return Err(VmError::ArityError {
                    expected: u8::try_from(params.len()).unwrap_or(u8::MAX),
                    got: u8::try_from(roots.len()).unwrap_or(u8::MAX),
                });
            }

            // Phase 1: Detach any shared COW `Bytes` buffers BEFORE taking raw pointers.
            for (root, &param) in roots.iter().zip(&params) {
                if param.is_mutable_bytes() {
                    ctx.prepare_ffi_bytes_mut(root)?;
                }
            }

            // Peel Newtypes, pre-validate scalars/CStrs, and snapshot reloaded `TaggedValue`
            // handles after all COW allocations have completed.
            let mut reloaded = Vec::with_capacity(roots.len());
            let mut predecoded = Vec::with_capacity(roots.len());
            let mut cstr_buffers: Vec<Vec<u8>> = Vec::new();
            for (root, &param) in roots.iter().zip(&params) {
                let raw_val = ctx.load_rooted(root)?;
                let val = ctx.peel_newtype(raw_val)?;
                predecoded.push(predecode_arg(ctx, val, param, &mut cstr_buffers)?);
                reloaded.push(val);
            }

            // Phase 2: Non-allocating native frame guard (`&Vm` immutable, `native_frame_depth > 0`).
            let keep_alive = backing;
            let raw_ret = ctx.with_native_ffi_frame(|vm| {
                let mut c_words = [CArgWord::Gp(0); MAX_C_WORDS];
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
                            c_words[cursor] = CArgWord::Gp(ptr as usize);
                            cursor += 1;
                        }
                        CParamKind::SliceRo => {
                            let (ptr, len) = vm.raw_ro_slice(val)?;
                            if len > 0 {
                                ro_ranges.push((ptr as usize, (ptr as usize).saturating_add(len)));
                            }
                            c_words[cursor] = CArgWord::Gp(ptr as usize);
                            c_words[cursor + 1] = CArgWord::Gp(len);
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
                            c_words[cursor] = CArgWord::Gp(ptr as usize);
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
                            c_words[cursor] = CArgWord::Gp(ptr as usize);
                            c_words[cursor + 1] = CArgWord::Gp(len);
                            cursor += 2;
                        }
                        _ => match predecoded[idx] {
                            PredecodedArg::Word(word) => {
                                c_words[cursor] = word;
                                cursor += 1;
                            }
                            PredecodedArg::CStr(buf_idx) => {
                                c_words[cursor] =
                                    CArgWord::Gp(cstr_buffers[buf_idx].as_ptr() as usize);
                                cursor += 1;
                            }
                            PredecodedArg::DeferredSlice => {
                                unreachable!("slice/pointer kinds matched above")
                            }
                        },
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

                // SAFETY: `fn_addr` belongs to a live library (`keep_alive`), `c_words` holds
                // validated C-ABI GP/FP words whose backing objects (`Bytes`, `String`, `cstr_buffers`)
                // remain live and unmoved across `with_native_ffi_frame`, and mutable `Bytes`
                // buffers are uniquely owned and non-aliasing.
                unsafe { dispatch_extern_c(fn_addr, callconv, &c_words[..total_c_words], ret) }
            })?;
            drop(cstr_buffers);

            // Phase 3: Marshal C return value back into exact-typed Nessa `TaggedValue`.
            let res = match ret {
                CReturnKind::Void => {
                    ctx.return_unit();
                    Ok(())
                }
                CReturnKind::I8 => ctx.return_i8(raw_ret.gp as i8),
                CReturnKind::U8 => ctx.return_u8(raw_ret.gp as u8),
                CReturnKind::I16 => ctx.return_i16(raw_ret.gp as i16),
                CReturnKind::U16 => ctx.return_u16(raw_ret.gp as u16),
                CReturnKind::I32 => ctx.return_i32(raw_ret.gp as i32),
                CReturnKind::U32 => ctx.return_u32(raw_ret.gp as u32),
                CReturnKind::I64 => ctx.return_i64(raw_ret.gp as i64),
                CReturnKind::U64 => ctx.return_u64(raw_ret.gp),
                CReturnKind::Isize => ctx.return_isize(raw_ret.gp as isize),
                CReturnKind::Usize => ctx.return_usize(raw_ret.gp as usize),
                CReturnKind::F32 => ctx.return_f32(f32::from_bits(raw_ret.fp as u32)),
                CReturnKind::F64 => ctx.return_f64(f64::from_bits(raw_ret.fp)),
                CReturnKind::Bool => {
                    ctx.return_bool((raw_ret.gp & 0xFF) != 0);
                    Ok(())
                }
                CReturnKind::Ptr => ctx.return_usize(raw_ret.gp as usize),
                CReturnKind::CStr => {
                    let addr = raw_ret.gp as usize;
                    if addr == 0 {
                        return Err(VmError::InvalidArgument(
                            "FFI function returned NULL for non-nullable CStr",
                        ));
                    }
                    let s = read_bounded_cstr(addr, MAX_FFI_CSTR_BYTES)?;
                    ctx.check_string_size(s.len())?;
                    ctx.return_string(&s)
                }
            };
            drop(keep_alive);
            res
        }
    }
}

#[cfg(all(target_arch = "x86_64", unix))]
#[repr(C)]
struct SysVCallFrame {
    gp: [u64; 6],
    xmm: [u64; 8],
    stack: [u64; 8],
    xmm_count: u64,
    fn_addr: u64,
    ret_gp: u64,
    ret_xmm: u64,
}

#[cfg(all(target_arch = "x86_64", unix))]
const _: () = {
    assert!(std::mem::offset_of!(SysVCallFrame, gp) == 0);
    assert!(std::mem::offset_of!(SysVCallFrame, xmm) == 48);
    assert!(std::mem::offset_of!(SysVCallFrame, stack) == 112);
    assert!(std::mem::offset_of!(SysVCallFrame, xmm_count) == 176);
    assert!(std::mem::offset_of!(SysVCallFrame, fn_addr) == 184);
    assert!(std::mem::offset_of!(SysVCallFrame, ret_gp) == 192);
    assert!(std::mem::offset_of!(SysVCallFrame, ret_xmm) == 200);
};

#[cfg(all(target_arch = "x86_64", unix))]
#[inline(never)]
unsafe fn call_sysv64(frame: &mut SysVCallFrame) {
    let frame_ptr = std::ptr::from_mut(frame);
    // SAFETY: `frame_ptr` points to a properly aligned `SysVCallFrame` on the Rust stack.
    // `r12` and `r13` are callee-saved in the System V AMD64 ABI (`clobber_abi("sysv64")`
    // preserves all Rust caller registers while `inout("r12")` and `out("r13")` save/restore
    // `r12`/`r13`). `rsp` is 16-byte aligned (`and rsp, -16` then `sub rsp, 64`) prior to
    // `call r11`, and restored verbatim from `r13` immediately after capturing `rax`/`xmm0`.
    unsafe {
        core::arch::asm!(
            "mov r13, rsp",
            "and rsp, -16",
            "sub rsp, 64",
            "mov rax, [r12 + 112]",
            "mov [rsp], rax",
            "mov rax, [r12 + 120]",
            "mov [rsp + 8], rax",
            "mov rax, [r12 + 128]",
            "mov [rsp + 16], rax",
            "mov rax, [r12 + 136]",
            "mov [rsp + 24], rax",
            "mov rax, [r12 + 144]",
            "mov [rsp + 32], rax",
            "mov rax, [r12 + 152]",
            "mov [rsp + 40], rax",
            "mov rax, [r12 + 160]",
            "mov [rsp + 48], rax",
            "mov rax, [r12 + 168]",
            "mov [rsp + 56], rax",
            "mov rax, [r12 + 48]",
            "movq xmm0, rax",
            "mov rax, [r12 + 56]",
            "movq xmm1, rax",
            "mov rax, [r12 + 64]",
            "movq xmm2, rax",
            "mov rax, [r12 + 72]",
            "movq xmm3, rax",
            "mov rax, [r12 + 80]",
            "movq xmm4, rax",
            "mov rax, [r12 + 88]",
            "movq xmm5, rax",
            "mov rax, [r12 + 96]",
            "movq xmm6, rax",
            "mov rax, [r12 + 104]",
            "movq xmm7, rax",
            "mov rdi, [r12]",
            "mov rsi, [r12 + 8]",
            "mov rdx, [r12 + 16]",
            "mov rcx, [r12 + 24]",
            "mov r8, [r12 + 32]",
            "mov r9, [r12 + 40]",
            "mov rax, [r12 + 176]",
            "mov r11, [r12 + 184]",
            "call r11",
            "mov [r12 + 192], rax",
            "movq rax, xmm0",
            "mov [r12 + 200], rax",
            "mov rsp, r13",
            inout("r12") frame_ptr => _,
            out("r13") _,
            clobber_abi("sysv64"),
        );
    }
}

#[cfg(all(target_arch = "x86_64", windows))]
#[repr(C)]
struct Win64CallFrame {
    gp: [u64; 4],
    xmm: [u64; 4],
    stack: [u64; 8],
    fn_addr: u64,
    ret_gp: u64,
    ret_xmm: u64,
}

#[cfg(all(target_arch = "x86_64", windows))]
const _: () = {
    assert!(std::mem::offset_of!(Win64CallFrame, gp) == 0);
    assert!(std::mem::offset_of!(Win64CallFrame, xmm) == 32);
    assert!(std::mem::offset_of!(Win64CallFrame, stack) == 64);
    assert!(std::mem::offset_of!(Win64CallFrame, fn_addr) == 128);
    assert!(std::mem::offset_of!(Win64CallFrame, ret_gp) == 136);
    assert!(std::mem::offset_of!(Win64CallFrame, ret_xmm) == 144);
};

#[cfg(all(target_arch = "x86_64", windows))]
#[inline(never)]
unsafe fn call_win64(frame: &mut Win64CallFrame) {
    let frame_ptr = std::ptr::from_mut(frame);
    // SAFETY: `frame_ptr` points to a properly aligned `Win64CallFrame` on the Rust stack.
    // `r12` and `r13` are non-volatile (callee-saved) in the Microsoft x64 ABI. `rsp` is
    // 16-byte aligned (`and rsp, -16` then `sub rsp, 96`) providing the mandatory 32-byte
    // shadow space `[rsp..rsp+32]` plus up to 8 stack slots `[rsp+32..rsp+96]` prior to `call r11`.
    unsafe {
        core::arch::asm!(
            "mov r13, rsp",
            "and rsp, -16",
            "sub rsp, 96",
            "xor eax, eax",
            "mov [rsp], rax",
            "mov [rsp + 8], rax",
            "mov [rsp + 16], rax",
            "mov [rsp + 24], rax",
            "mov rax, [r12 + 64]",
            "mov [rsp + 32], rax",
            "mov rax, [r12 + 72]",
            "mov [rsp + 40], rax",
            "mov rax, [r12 + 80]",
            "mov [rsp + 48], rax",
            "mov rax, [r12 + 88]",
            "mov [rsp + 56], rax",
            "mov rax, [r12 + 96]",
            "mov [rsp + 64], rax",
            "mov rax, [r12 + 104]",
            "mov [rsp + 72], rax",
            "mov rax, [r12 + 112]",
            "mov [rsp + 80], rax",
            "mov rax, [r12 + 120]",
            "mov [rsp + 88], rax",
            "mov rax, [r12 + 32]",
            "movq xmm0, rax",
            "mov rax, [r12 + 40]",
            "movq xmm1, rax",
            "mov rax, [r12 + 48]",
            "movq xmm2, rax",
            "mov rax, [r12 + 56]",
            "movq xmm3, rax",
            "mov rcx, [r12]",
            "mov rdx, [r12 + 8]",
            "mov r8, [r12 + 16]",
            "mov r9, [r12 + 24]",
            "mov r11, [r12 + 128]",
            "call r11",
            "mov [r12 + 136], rax",
            "movq rax, xmm0",
            "mov [r12 + 144], rax",
            "mov rsp, r13",
            inout("r12") frame_ptr => _,
            out("r13") _,
            clobber_abi("win64"),
        );
    }
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
struct Aarch64CallFrame {
    gp: [u64; 8],
    fp: [u64; 8],
    stack: [u64; 8],
    fn_addr: u64,
    ret_gp: u64,
    ret_fp: u64,
}

#[cfg(target_arch = "aarch64")]
const _: () = {
    assert!(std::mem::offset_of!(Aarch64CallFrame, gp) == 0);
    assert!(std::mem::offset_of!(Aarch64CallFrame, fp) == 64);
    assert!(std::mem::offset_of!(Aarch64CallFrame, stack) == 128);
    assert!(std::mem::offset_of!(Aarch64CallFrame, fn_addr) == 192);
    assert!(std::mem::offset_of!(Aarch64CallFrame, ret_gp) == 200);
    assert!(std::mem::offset_of!(Aarch64CallFrame, ret_fp) == 208);
};

#[cfg(target_arch = "aarch64")]
#[inline(never)]
unsafe fn call_aarch64(frame: &mut Aarch64CallFrame) {
    let frame_ptr = std::ptr::from_mut(frame);
    // SAFETY: `frame_ptr` points to a properly aligned `Aarch64CallFrame` on the Rust stack.
    // `x20` is callee-saved in AAPCS64 across all OS targets; `sp` remains 16-byte aligned
    // throughout (`sub sp, sp, #64` and `add sp, sp, #64`), and `clobber_abi("C")` preserves
    // all caller-saved GP and SIMD/FP registers.
    unsafe {
        core::arch::asm!(
            "sub sp, sp, #64",
            "ldr x9, [x20, #128]",
            "str x9, [sp, #0]",
            "ldr x9, [x20, #136]",
            "str x9, [sp, #8]",
            "ldr x9, [x20, #144]",
            "str x9, [sp, #16]",
            "ldr x9, [x20, #152]",
            "str x9, [sp, #24]",
            "ldr x9, [x20, #160]",
            "str x9, [sp, #32]",
            "ldr x9, [x20, #168]",
            "str x9, [sp, #40]",
            "ldr x9, [x20, #176]",
            "str x9, [sp, #48]",
            "ldr x9, [x20, #184]",
            "str x9, [sp, #56]",
            "ldr d0, [x20, #64]",
            "ldr d1, [x20, #72]",
            "ldr d2, [x20, #80]",
            "ldr d3, [x20, #88]",
            "ldr d4, [x20, #96]",
            "ldr d5, [x20, #104]",
            "ldr d6, [x20, #112]",
            "ldr d7, [x20, #120]",
            "ldr x0, [x20, #0]",
            "ldr x1, [x20, #8]",
            "ldr x2, [x20, #16]",
            "ldr x3, [x20, #24]",
            "ldr x4, [x20, #32]",
            "ldr x5, [x20, #40]",
            "ldr x6, [x20, #48]",
            "ldr x7, [x20, #56]",
            "ldr x16, [x20, #192]",
            "blr x16",
            "str x0, [x20, #200]",
            "str d0, [x20, #208]",
            "add sp, sp, #64",
            inout("x20") frame_ptr => _,
            clobber_abi("C"),
        );
    }
}

unsafe fn dispatch_extern_c(
    fn_addr: usize,
    _callconv: CallConv,
    words: &[CArgWord],
    _ret: CReturnKind,
) -> Result<RawCReturn, VmError> {
    debug_assert!(words.len() <= MAX_C_WORDS);

    #[cfg(all(target_arch = "x86_64", unix))]
    {
        let mut frame = SysVCallFrame {
            gp: [0; 6],
            xmm: [0; 8],
            stack: [0; 8],
            xmm_count: 0,
            fn_addr: fn_addr as u64,
            ret_gp: 0,
            ret_xmm: 0,
        };
        let mut gp_idx = 0usize;
        let mut xmm_idx = 0usize;
        let mut stack_idx = 0usize;
        for &word in words {
            match word {
                CArgWord::Gp(v) => {
                    if gp_idx < 6 {
                        frame.gp[gp_idx] = v as u64;
                        gp_idx += 1;
                    } else {
                        frame.stack[stack_idx] = v as u64;
                        stack_idx += 1;
                    }
                }
                CArgWord::F32(bits) => {
                    if xmm_idx < 8 {
                        frame.xmm[xmm_idx] = u64::from(bits);
                        xmm_idx += 1;
                    } else {
                        frame.stack[stack_idx] = u64::from(bits);
                        stack_idx += 1;
                    }
                }
                CArgWord::F64(bits) => {
                    if xmm_idx < 8 {
                        frame.xmm[xmm_idx] = bits;
                        xmm_idx += 1;
                    } else {
                        frame.stack[stack_idx] = bits;
                        stack_idx += 1;
                    }
                }
            }
        }
        frame.xmm_count = xmm_idx as u64;
        // SAFETY: `frame` is populated according to the System V AMD64 register/stack rules
        // and `fn_addr` is a valid foreign function pointer verified by the caller.
        unsafe {
            call_sysv64(&mut frame);
        }
        Ok(RawCReturn {
            gp: frame.ret_gp,
            fp: frame.ret_xmm,
        })
    }

    #[cfg(all(target_arch = "x86_64", windows))]
    {
        let mut frame = Win64CallFrame {
            gp: [0; 4],
            xmm: [0; 4],
            stack: [0; 8],
            fn_addr: fn_addr as u64,
            ret_gp: 0,
            ret_xmm: 0,
        };
        for (idx, &word) in words.iter().enumerate() {
            if idx < 4 {
                match word {
                    CArgWord::Gp(v) => {
                        frame.gp[idx] = v as u64;
                    }
                    CArgWord::F32(bits) => {
                        let raw = u64::from(bits);
                        frame.xmm[idx] = raw;
                        frame.gp[idx] = raw;
                    }
                    CArgWord::F64(bits) => {
                        frame.xmm[idx] = bits;
                        frame.gp[idx] = bits;
                    }
                }
            } else {
                frame.stack[idx - 4] = match word {
                    CArgWord::Gp(v) => v as u64,
                    CArgWord::F32(bits) => u64::from(bits),
                    CArgWord::F64(bits) => bits,
                };
            }
        }
        // SAFETY: `frame` is populated according to the Microsoft x64 ABI positional slot
        // and 32-byte shadow-space rules; `"c"` and `"system"` are identical on x86_64 Windows.
        unsafe {
            call_win64(&mut frame);
        }
        Ok(RawCReturn {
            gp: frame.ret_gp,
            fp: frame.ret_xmm,
        })
    }

    #[cfg(target_arch = "aarch64")]
    {
        let mut frame = Aarch64CallFrame {
            gp: [0; 8],
            fp: [0; 8],
            stack: [0; 8],
            fn_addr: fn_addr as u64,
            ret_gp: 0,
            ret_fp: 0,
        };
        let mut gp_idx = 0usize;
        let mut fp_idx = 0usize;
        let mut stack_idx = 0usize;
        for &word in words {
            match word {
                CArgWord::Gp(v) => {
                    if gp_idx < 8 {
                        frame.gp[gp_idx] = v as u64;
                        gp_idx += 1;
                    } else {
                        frame.stack[stack_idx] = v as u64;
                        stack_idx += 1;
                    }
                }
                CArgWord::F32(bits) => {
                    if fp_idx < 8 {
                        frame.fp[fp_idx] = u64::from(bits);
                        fp_idx += 1;
                    } else {
                        frame.stack[stack_idx] = u64::from(bits);
                        stack_idx += 1;
                    }
                }
                CArgWord::F64(bits) => {
                    if fp_idx < 8 {
                        frame.fp[fp_idx] = bits;
                        fp_idx += 1;
                    } else {
                        frame.stack[stack_idx] = bits;
                        stack_idx += 1;
                    }
                }
            }
        }
        // SAFETY: `frame` is populated according to AAPCS64 (`x0..x7`, `d0..d7`, stack slots)
        // and `fn_addr` is a valid foreign function pointer verified by the caller.
        unsafe {
            call_aarch64(&mut frame);
        }
        Ok(RawCReturn {
            gp: frame.ret_gp,
            fp: frame.ret_fp,
        })
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        dispatch_extern_c_fallback(fn_addr, words, _ret)
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
unsafe fn dispatch_extern_c_fallback(
    fn_addr: usize,
    words: &[CArgWord],
    ret: CReturnKind,
) -> Result<RawCReturn, VmError> {
    match (words, ret) {
        (&[CArgWord::F32(x)], CReturnKind::F32) => {
            // SAFETY: Caller verified `fn_addr` matches `extern "C" fn(f32) -> f32`.
            let r = unsafe {
                (std::mem::transmute::<usize, unsafe extern "C" fn(f32) -> f32>(fn_addr))(
                    f32::from_bits(x),
                )
            };
            return Ok(RawCReturn {
                gp: 0,
                fp: u64::from(r.to_bits()),
            });
        }
        (&[CArgWord::F32(x), CArgWord::F32(y)], CReturnKind::F32) => {
            // SAFETY: Caller verified `fn_addr` matches `extern "C" fn(f32, f32) -> f32`.
            let r = unsafe {
                (std::mem::transmute::<usize, unsafe extern "C" fn(f32, f32) -> f32>(fn_addr))(
                    f32::from_bits(x),
                    f32::from_bits(y),
                )
            };
            return Ok(RawCReturn {
                gp: 0,
                fp: u64::from(r.to_bits()),
            });
        }
        (&[CArgWord::F64(x)], CReturnKind::F64) => {
            // SAFETY: Caller verified `fn_addr` matches `extern "C" fn(f64) -> f64`.
            let r = unsafe {
                (std::mem::transmute::<usize, unsafe extern "C" fn(f64) -> f64>(fn_addr))(
                    f64::from_bits(x),
                )
            };
            return Ok(RawCReturn {
                gp: 0,
                fp: r.to_bits(),
            });
        }
        (&[CArgWord::F64(x), CArgWord::F64(y)], CReturnKind::F64) => {
            // SAFETY: Caller verified `fn_addr` matches `extern "C" fn(f64, f64) -> f64`.
            let r = unsafe {
                (std::mem::transmute::<usize, unsafe extern "C" fn(f64, f64) -> f64>(fn_addr))(
                    f64::from_bits(x),
                    f64::from_bits(y),
                )
            };
            return Ok(RawCReturn {
                gp: 0,
                fp: r.to_bits(),
            });
        }
        _ => {}
    }

    if matches!(ret, CReturnKind::F32 | CReturnKind::F64) {
        return Err(VmError::InvalidArgument(
            "floating-point FFI signature requires x86_64 or aarch64 C ABI dispatcher",
        ));
    }

    let mut w = [0usize; MAX_C_WORDS];
    for (idx, &word) in words.iter().enumerate() {
        match word {
            CArgWord::Gp(v) => w[idx] = v,
            CArgWord::F32(_) | CArgWord::F64(_) => {
                return Err(VmError::InvalidArgument(
                    "floating-point FFI parameter requires x86_64 or aarch64 C ABI dispatcher",
                ));
            }
        }
    }
    let arity = words.len();
    if ret == CReturnKind::Void {
        // SAFETY: Void-returning pure-GP C functions must not have an undefined return register read.
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
        Ok(RawCReturn { gp: 0, fp: 0 })
    } else {
        // SAFETY: Integer/pointer-returning pure-GP C functions return in the primary word register.
        let gp = unsafe {
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
        };
        Ok(RawCReturn {
            gp: gp as u64,
            fp: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_parser_handles_compact_nessa_fn_callconv_and_gc_hook() {
        assert_eq!(
            parse_signature("on_gc:any,i64->void").unwrap(),
            SymbolKind::GcCleanupI64
        );
        assert_eq!(
            parse_signature("slice_ro,u32,usize,usize,buf_mut->i64").unwrap(),
            SymbolKind::CFunction {
                callconv: CallConv::C,
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
                callconv: CallConv::C,
                params: vec![],
                total_c_words: 0,
                ret: CReturnKind::Void,
            }
        );
        assert_eq!(
            parse_signature("c:fn(f32) -> f32").unwrap(),
            SymbolKind::CFunction {
                callconv: CallConv::C,
                params: vec![CParamKind::F32],
                total_c_words: 1,
                ret: CReturnKind::F32,
            }
        );
        assert_eq!(
            parse_signature("system:fn(i8, u8, i16, u16, Ptr, CStr) -> Ptr").unwrap(),
            SymbolKind::CFunction {
                callconv: CallConv::System,
                params: vec![
                    CParamKind::I8,
                    CParamKind::U8,
                    CParamKind::I16,
                    CParamKind::U16,
                    CParamKind::Ptr,
                    CParamKind::CStr,
                ],
                total_c_words: 6,
                ret: CReturnKind::Ptr,
            }
        );
        assert_eq!(
            parse_signature("fn() -> Unit").unwrap(),
            SymbolKind::CFunction {
                callconv: CallConv::C,
                params: vec![],
                total_c_words: 0,
                ret: CReturnKind::Void,
            }
        );
        assert_eq!(
            parse_signature("fn(SliceRo, u32, usize, usize, BufMut) -> i64").unwrap(),
            SymbolKind::CFunction {
                callconv: CallConv::C,
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
        assert!(parse_signature("unknown->i64").is_err());
        assert!(parse_signature("fastcall:fn() -> Unit").is_err());
        assert!(parse_signature("i64").is_err());
    }

    #[test]
    fn bounded_cstr_reader_validates_nul_and_utf8() {
        let valid = b"hello ffi\0trailing";
        assert_eq!(
            read_bounded_cstr(valid.as_ptr() as usize, 64).unwrap(),
            "hello ffi"
        );
        assert!(read_bounded_cstr(valid.as_ptr() as usize, 9).is_err());

        let bad_utf8 = [0xFFu8, 0xFE, 0x00];
        assert!(read_bounded_cstr(bad_utf8.as_ptr() as usize, 16).is_err());
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn hardware_c_abi_dispatcher_handles_gp_float_narrow_cstr_and_stack_spills() {
        let map = lookup_host_library("__nessa_ffi_test").expect("builtin test host lib");

        // 1. i32 add
        let add_addr = map["test_add_i32"];
        // SAFETY: `add_addr` is `extern "C" fn(i32, i32) -> i32` with matching args.
        let r_add = unsafe {
            dispatch_extern_c(
                add_addr,
                CallConv::C,
                &[CArgWord::Gp((-10isize) as usize), CArgWord::Gp(52)],
                CReturnKind::I32,
            )
            .unwrap()
        };
        assert_eq!(r_add.gp as i32, 42);

        // 2. Narrow signed/unsigned mix
        let mix_addr = map["test_narrow_mix"];
        // SAFETY: `mix_addr` is `extern "C" fn(i8, u8, i16, u16) -> i32` with sign/zero-extended args.
        let r_mix = unsafe {
            dispatch_extern_c(
                mix_addr,
                CallConv::C,
                &[
                    CArgWord::Gp((-5i8 as isize) as usize),
                    CArgWord::Gp(200usize),
                    CArgWord::Gp((-1000i16 as isize) as usize),
                    CArgWord::Gp(40_000usize),
                ],
                CReturnKind::I32,
            )
            .unwrap()
        };
        assert_eq!(r_mix.gp as i32, 39_195);

        // 3. f32 math (3-4-5 right triangle)
        let f32_addr = map["test_f32_math"];
        // SAFETY: `f32_addr` is `extern "C" fn(f32, f32) -> f32`.
        let r_f32 = unsafe {
            dispatch_extern_c(
                f32_addr,
                CallConv::C,
                &[
                    CArgWord::F32(3.0f32.to_bits()),
                    CArgWord::F32(4.0f32.to_bits()),
                ],
                CReturnKind::F32,
            )
            .unwrap()
        };
        assert!((f32::from_bits(r_f32.fp as u32) - 5.0f32).abs() < 1e-6);

        // 4. Interleaved GP + f64 + f32 + i64 -> f64
        let f64_addr = map["test_f64_mix"];
        // SAFETY: `f64_addr` is `extern "C" fn(i32, f64, f32, i64) -> f64`.
        let r_f64 = unsafe {
            dispatch_extern_c(
                f64_addr,
                CallConv::C,
                &[
                    CArgWord::Gp(3),
                    CArgWord::F64(2.5f64.to_bits()),
                    CArgWord::F32(1.25f32.to_bits()),
                    CArgWord::Gp(10),
                ],
                CReturnKind::F64,
            )
            .unwrap()
        };
        assert!((f64::from_bits(r_f64.fp) - 18.75).abs() < 1e-9);

        // 5. 8-arg mixed GP/XMM/stack spill (`test_many_args`)
        let many_addr = map["test_many_args"];
        // SAFETY: `many_addr` is `extern "C" fn(i64, f64, i32, f32, i64, f64, i32, i64) -> f64`.
        let r_many = unsafe {
            dispatch_extern_c(
                many_addr,
                CallConv::C,
                &[
                    CArgWord::Gp(1),
                    CArgWord::F64(2.5f64.to_bits()),
                    CArgWord::Gp((-3i32 as isize) as usize),
                    CArgWord::F32(4.5f32.to_bits()),
                    CArgWord::Gp(10),
                    CArgWord::F64(20.25f64.to_bits()),
                    CArgWord::Gp(30),
                    CArgWord::Gp(40),
                ],
                CReturnKind::F64,
            )
            .unwrap()
        };
        assert!((f64::from_bits(r_many.fp) - 105.25).abs() < 1e-9);

        // 6. CStr greeting & length
        let greet_addr = map["test_cstr_greeting"];
        let len_addr = map["test_cstr_len"];
        // SAFETY: `greet_addr` takes no args and returns a static `const char*`.
        let r_greet =
            unsafe { dispatch_extern_c(greet_addr, CallConv::C, &[], CReturnKind::CStr).unwrap() };
        assert_eq!(
            read_bounded_cstr(r_greet.gp as usize, 64).unwrap(),
            "hello from c"
        );
        // SAFETY: `len_addr` takes `const char*` and returns `usize`.
        let r_len = unsafe {
            dispatch_extern_c(
                len_addr,
                CallConv::C,
                &[CArgWord::Gp(r_greet.gp as usize)],
                CReturnKind::Usize,
            )
            .unwrap()
        };
        assert_eq!(r_len.gp as usize, 12);
    }
}
