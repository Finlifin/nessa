//! Conversion / introspection builtins.

use interpreter::{BuiltinCtx, VmError};

pub fn type_of(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let ty = ctx.arg_value_type(0)?;
    ctx.return_type(ty)
}

pub fn to_i64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx
        .arg_number(0)?
        .to_i64_checked()
        .ok_or(VmError::NumericOverflow)?;
    ctx.return_i64(value)
}

pub fn to_f64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_number(0)?.to_f64();
    ctx.return_f64(value)
}

pub fn to_string(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let s = ctx.format_value(val)?;
    ctx.return_string(&s)
}

/// Primitive equality used by explicitly typed standard-library trait bodies.
pub fn scalar_eq(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg(0)?;
    let right = ctx.arg(1)?;
    let equal = ctx.scalar_equal(left, right)?;
    ctx.return_bool(equal);
    Ok(())
}

/// Internal done test for `for` loops over dynamically typed iterables.
pub fn iteration_step_done(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let step = ctx.arg(0)?;
    let done = ctx.iteration_step_done(step)?;
    ctx.return_bool(done);
    Ok(())
}

/// Internal native step of the standard library's Any equality.
pub fn native_equal(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg(0)?;
    let right = ctx.arg(1)?;
    match ctx.native_equal(left, right)? {
        Some(equal) => ctx.return_bool(equal),
        None => ctx.return_null(),
    }
    Ok(())
}

pub fn tuple_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg(0)?;
    match ctx.tuple_length(value)? {
        Some(length) => {
            let length = i64::try_from(length).map_err(|_| VmError::NumericOverflow)?;
            ctx.return_i64(length)
        }
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn tuple_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let tuple = ctx.arg(0)?;
    let index = ctx.arg_i64(1)?;
    let element = ctx.tuple_element(tuple, index)?;
    ctx.set_return(element)
}

/// Internal partial ordering result consumed by the standard-library Ordering adapter.
pub fn scalar_cmp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg(0)?;
    let right = ctx.arg(1)?;
    match ctx.scalar_partial_cmp(left, right)? {
        Some(std::cmp::Ordering::Less) => ctx.return_i64(-1),
        Some(std::cmp::Ordering::Equal) => ctx.return_i64(0),
        Some(std::cmp::Ordering::Greater) => ctx.return_i64(1),
        None => ctx.set_return(runtime::TaggedValue::NULL),
    }
}

/// Returns `true` iff the argument is a heap `Err` `ErrorEnvelope` (`PayloadRole::ErrorEnvelope` with non-zero tag).
pub fn is_error_value(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let root = ctx.arg_rooted(0)?;
    let val = ctx.load_rooted(&root)?;
    ctx.return_bool(ctx.is_error_value(val));
    Ok(())
}

/// Unwraps `Ok(payload)` from a heap `ErrorEnvelope` when its error tag is zero, or returns `val` unchanged.
pub fn unwrap_ok_value(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let root = ctx.arg_rooted(0)?;
    let val = ctx.load_rooted(&root)?;
    let unwrapped = ctx.unwrap_ok_value(val)?;
    ctx.set_return(unwrapped)
}

/// Verifies `val` is a heap `Err` `ErrorEnvelope` (non-zero error tag) and returns it unchanged.
pub fn forward_error_value(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let root = ctx.arg_rooted(0)?;
    let val = ctx.load_rooted(&root)?;
    if !ctx.is_error_value(val) {
        return Err(VmError::TypeError);
    }
    ctx.set_return(val)
}
