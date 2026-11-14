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

pub fn map_values(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_map_values(ctx.arg(0)?)
}

pub fn map_clear(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.map_clear(ctx.arg(0)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn map_merge(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.map_merge(ctx.arg(0)?, ctx.arg(1)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn map_get_or_insert(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let value = ctx.map_get_or_insert(ctx.arg(0)?, ctx.arg(1)?, ctx.arg(2)?)?;
    ctx.set_return(value)
}

pub fn map_insert_absent(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let inserted = ctx.map_insert_absent(ctx.arg(0)?, ctx.arg(1)?, ctx.arg(2)?)?;
    ctx.return_bool(inserted);
    Ok(())
}

pub fn map_entries(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_map_entries(ctx.arg(0)?)
}

pub fn map_is_empty(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(ctx.map_len(ctx.arg(0)?)? == 0);
    Ok(())
}

pub fn map_copy(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.map_copy(ctx.arg(0)?)
}
