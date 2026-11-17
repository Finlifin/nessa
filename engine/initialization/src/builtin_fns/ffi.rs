//! Builtin functions for the capability-gated C FFI (`__ffi_dlopen`..`__ffi_read_cstr`).

use interpreter::{BuiltinCtx, VmError};

pub fn ffi_dlopen(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ::ffi::ffi_dlopen(ctx)
}

pub fn ffi_dlsym(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ::ffi::ffi_dlsym(ctx)
}

pub fn ffi_call(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ::ffi::ffi_call(ctx)
}

pub fn ffi_call4(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ::ffi::ffi_call4(ctx)
}

pub fn ffi_call6(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ::ffi::ffi_call6(ctx)
}

pub fn ffi_read_cstr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ::ffi::ffi_read_cstr(ctx)
}
