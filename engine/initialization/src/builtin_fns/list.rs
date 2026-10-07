//! Checked List operations through the rooted native-call context.

use interpreter::{BuiltinCtx, VmError};

pub fn list_init(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_empty_list()
}

pub fn list_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg(0)?;
    let length = ctx.list_len(list)?;
    ctx.return_i64(i64::try_from(length).map_err(|_| VmError::NumericOverflow)?)
}

pub fn list_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let list = ctx.arg(0)?;
    let index = ctx.arg(1)?;
    let value = ctx.list_get(list, index)?;
    ctx.set_return(value)
}

pub fn list_set(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let list = ctx.arg(0)?;
    let index = ctx.arg(1)?;
    let value = ctx.arg(2)?;
    ctx.list_set(list, index, value)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_push(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let list = ctx.arg(0)?;
    let value = ctx.arg(1)?;
    ctx.list_push(list, value)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_pop(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg(0)?;
    let value = ctx.list_pop(list)?;
    ctx.set_return(value)
}
