//! String-keyed Map operations preserve arguments and results in the native root domain.

use interpreter::{BuiltinCtx, VmError};

pub fn map_init(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_empty_map()
}

pub fn map_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let map = ctx.arg(0)?;
    let length = ctx.map_len(map)?;
    ctx.return_i64(i64::try_from(length).map_err(|_| VmError::NumericOverflow)?)
}

pub fn map_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let map = ctx.arg(0)?;
    let key = ctx.arg(1)?;
    let value = ctx.map_get(map, key)?;
    ctx.set_return(value)
}

pub fn map_set(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let map = ctx.arg(0)?;
    let key = ctx.arg(1)?;
    let value = ctx.arg(2)?;
    ctx.map_set(map, key, value)?;
    ctx.return_unit();
    Ok(())
}

pub fn map_remove(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let map = ctx.arg(0)?;
    let key = ctx.arg(1)?;
    let value = ctx.map_remove(map, key)?;
    ctx.set_return(value)
}

pub fn map_contains(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let map = ctx.arg(0)?;
    let key = ctx.arg(1)?;
    let contains = ctx.map_contains(map, key)?;
    ctx.return_bool(contains);
    Ok(())
}

pub fn map_keys(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let map = ctx.arg(0)?;
    ctx.return_map_keys(map)
}
