//! String builtins.

use interpreter::{BuiltinCtx, VmError};
use runtime::TaggedValue;

pub fn str_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let ptr = val.as_heap_ptr().ok_or(VmError::TypeError)?;
    let len = unsafe { *(ptr as *const u64) } as i64;
    ctx.return_i64(len);
    Ok(())
}

pub fn str_concat(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let a = ctx.arg(0)?;
    let b = ctx.arg(1)?;
    let mut bytes: Vec<u8> = Vec::new();
    append_heap_string_bytes(a, &mut bytes)?;
    append_heap_string_bytes(b, &mut bytes)?;
    let s = String::from_utf8(bytes).map_err(|_| VmError::TypeError)?;
    let result = ctx.alloc_string(&s);
    ctx.set_return(result);
    Ok(())
}

fn append_heap_string_bytes(val: TaggedValue, out: &mut Vec<u8>) -> Result<(), VmError> {
    let ptr = val.as_heap_ptr().ok_or(VmError::TypeError)?;
    let len = unsafe { *(ptr as *const u64) } as usize;
    if len > 64 * 1024 * 1024 {
        return Err(VmError::TypeError);
    }
    let data = unsafe { std::slice::from_raw_parts((ptr as *const u8).add(8), len) };
    out.extend_from_slice(data);
    Ok(())
}
