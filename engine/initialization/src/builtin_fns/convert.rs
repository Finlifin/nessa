//! Conversion / introspection builtins.

use interpreter::{BuiltinCtx, VmError};
use runtime::TaggedValue;

pub fn type_of(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    // Type descriptors as first-class values are not fully wired yet.
    // Return unit until Type values are available; callers should not rely on this.
    let _ = ctx.arg(0)?;
    ctx.return_unit();
    Ok(())
}

pub fn to_i64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let result = if let Some(v) = val.as_i64() {
        TaggedValue::from_i64(v)
    } else if let Some(v) = val.as_u64() {
        TaggedValue::from_i64(v as i64)
    } else if let Some(v) = val.as_f64() {
        TaggedValue::from_i64(v as i64)
    } else {
        return Err(VmError::TypeError);
    };
    ctx.set_return(result);
    Ok(())
}

pub fn to_f64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let result = if let Some(v) = val.as_f64() {
        TaggedValue::from_f64(v)
    } else if let Some(v) = val.as_i64() {
        TaggedValue::from_f64(v as f64)
    } else if let Some(v) = val.as_u64() {
        TaggedValue::from_f64(v as f64)
    } else {
        return Err(VmError::TypeError);
    };
    ctx.set_return(result);
    Ok(())
}

pub fn to_string(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let s = ctx.format_value(val);
    let result = ctx.alloc_string(&s);
    ctx.set_return(result);
    Ok(())
}
