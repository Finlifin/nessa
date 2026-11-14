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

pub fn list_windows(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_windows(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_chunks(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_chunks(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_chunks_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_chunks_exact(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_rchunks(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_rchunks(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_rchunks_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_rchunks_exact(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_chunks_remainder(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_chunks_remainder(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_rchunks_remainder(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_rchunks_remainder(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_split_at(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_split_at(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_rotate_left(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_rotate_left(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_rotate_right(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_rotate_right(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_repeat(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let list = ctx.arg(0)?;
    let count = usize::try_from(ctx.arg_i64(1)?)
        .map_err(|_| VmError::InvalidArgument("repeat count must not be negative"))?;
    ctx.list_repeat(list, count)
}

pub fn list_truncate(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_truncate(ctx.arg(0)?, ctx.arg(1)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_fill(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_fill(ctx.arg(0)?, ctx.arg(1)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_swap_remove(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = ctx.list_swap_remove(ctx.arg(0)?, ctx.arg(1)?)?;
    ctx.set_return(value)
}

pub fn list_split_off(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_split_off(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_resize(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.list_resize(ctx.arg(0)?, ctx.arg(1)?, ctx.arg(2)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_copy(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.list_copy(ctx.arg(0)?)
}

pub fn list_reverse(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.list_reverse(ctx.arg(0)?)
}

pub fn list_concat(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_concat(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_swap(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.list_swap(ctx.arg(0)?, ctx.arg(1)?, ctx.arg(2)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_clear(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.list_clear(ctx.arg(0)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_insert(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.list_insert(ctx.arg(0)?, ctx.arg(1)?, ctx.arg(2)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_remove(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = ctx.list_remove(ctx.arg(0)?, ctx.arg(1)?)?;
    ctx.set_return(value)
}

pub fn list_take(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_take(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_drop(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_drop(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_first(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.list_first(ctx.arg(0)?)?;
    ctx.set_return(value)
}

pub fn list_last(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.list_last(ctx.arg(0)?)?;
    ctx.set_return(value)
}

fn checked_range_len(start: i64, end: i64, inclusive: bool) -> Result<i64, VmError> {
    if inclusive {
        if end < start {
            Ok(0)
        } else {
            end.checked_sub(start)
                .and_then(|span| span.checked_add(1))
                .ok_or(VmError::NumericOverflow)
        }
    } else if end <= start {
        Ok(0)
    } else {
        end.checked_sub(start).ok_or(VmError::NumericOverflow)
    }
}

pub fn range_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_i64(checked_range_len(ctx.arg_i64(0)?, ctx.arg_i64(1)?, false)?)
}

pub fn range_inclusive_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_i64(checked_range_len(ctx.arg_i64(0)?, ctx.arg_i64(1)?, true)?)
}

pub fn range_is_empty(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(checked_range_len(ctx.arg_i64(0)?, ctx.arg_i64(1)?, false)? == 0);
    Ok(())
}

pub fn range_inclusive_is_empty(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(checked_range_len(ctx.arg_i64(0)?, ctx.arg_i64(1)?, true)? == 0);
    Ok(())
}

pub fn range_contains(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let start = ctx.arg_i64(0)?;
    let end = ctx.arg_i64(1)?;
    let value = ctx.arg_i64(2)?;
    ctx.return_bool(start <= value && value < end);
    Ok(())
}

pub fn range_inclusive_contains(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let start = ctx.arg_i64(0)?;
    let end = ctx.arg_i64(1)?;
    let value = ctx.arg_i64(2)?;
    ctx.return_bool(start <= value && value <= end);
    Ok(())
}

pub fn range_from_contains(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(ctx.arg_i64(0)? <= ctx.arg_i64(1)?);
    Ok(())
}

pub fn range_to_contains(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(ctx.arg_i64(1)? < ctx.arg_i64(0)?);
    Ok(())
}

pub fn range_to_inclusive_contains(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(ctx.arg_i64(1)? <= ctx.arg_i64(0)?);
    Ok(())
}

pub fn list_append(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_append(ctx.arg(0)?, ctx.arg(1)?)?;
    ctx.return_unit();
    Ok(())
}

pub fn list_join(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_join(ctx.arg(0)?, ctx.arg(1)?)
}

pub fn list_is_empty(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(ctx.list_len(ctx.arg(0)?)? == 0);
    Ok(())
}

pub fn list_enumerate(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.list_enumerate(ctx.arg(0)?)
}

pub fn list_flatten(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.list_flatten(ctx.arg(0)?)
}

pub fn list_zip(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.list_zip(ctx.arg(0)?, ctx.arg(1)?)
}
