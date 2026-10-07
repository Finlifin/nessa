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

/// Internal entry point used by compiler-generated Display wrappers.
pub fn derived_display(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let receiver = ctx.arg(0)?;
    ctx.return_derived_display(receiver)
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
