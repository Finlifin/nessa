//! Builtin functions for `std.wasm` (`__wasm_validate`..`__wasm_close`).

use interpreter::{BuiltinCtx, VmError, wasm_bridge};

pub fn validate(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_validate(ctx)
}

pub fn inspect(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_inspect(ctx)
}

pub fn compile(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_compile(ctx)
}

pub fn module_exports(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_module_exports(ctx)
}

pub fn module_imports(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_module_imports(ctx)
}

pub fn instantiate(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_instantiate(ctx)
}

pub fn call(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_call(ctx)
}

pub fn mem_pages(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_mem_pages(ctx)
}

pub fn mem_grow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_mem_grow(ctx)
}

pub fn mem_read(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_mem_read(ctx)
}

pub fn mem_write(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_mem_write(ctx)
}

pub fn mem_read_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_mem_read_str(ctx)
}

pub fn mem_write_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_mem_write_str(ctx)
}

pub fn global_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_global_get(ctx)
}

pub fn global_set(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_global_set(ctx)
}

pub fn fuel(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_fuel(ctx)
}

pub fn run_wasi(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_run_wasi(ctx)
}

pub fn close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wasm_bridge::wasm_close(ctx)
}
