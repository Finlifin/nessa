//! Process builtins: exit, panic.

use interpreter::{BuiltinCtx, VmError};

pub fn exit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let code = ctx.arg_i64(0)? as i32;
    ctx.exit(code);
    Ok(())
}

pub fn panic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let msg = ctx.format_value(val);
    ctx.panic(msg);
    Ok(())
}
