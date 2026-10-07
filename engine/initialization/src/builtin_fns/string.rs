//! String builtins.

use interpreter::{BuiltinCtx, VmError};

pub fn str_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let len = ctx.arg_string(0)?.len() as i64;
    ctx.return_i64(len)?;
    Ok(())
}

pub fn str_concat(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let mut s = ctx.arg_string(0)?;
    let right = ctx.arg_string(1)?;
    let bytes = s
        .len()
        .checked_add(right.len())
        .ok_or(VmError::DisplaySizeExceeded)?;
    ctx.check_display_size(bytes)?;
    s.push_str(&right);
    ctx.return_string(&s)
}

/// Preserve literal quoting for String-typed aggregate components.
pub fn display_quote(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_string(0)?;
    let quoted = format!("{value:?}");
    ctx.return_string(&quoted)
}

pub fn display_enter_tuple(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg(0)?;
    let entered = ctx.display_tuple_enter(value)?;
    ctx.return_bool(entered);
    Ok(())
}

pub fn display_exit_tuple(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg(0)?;
    ctx.display_tuple_exit(value)?;
    ctx.set_return(runtime::TaggedValue::UNIT)
}
