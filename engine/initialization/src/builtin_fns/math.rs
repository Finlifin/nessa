//! Math builtins.

use interpreter::{BuiltinCtx, VmError};
use runtime::TaggedValue;

pub fn abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let result = if let Some(v) = val.as_i64() {
        TaggedValue::from_i64(v.wrapping_abs())
    } else if let Some(v) = val.as_f64() {
        TaggedValue::from_f64(v.abs())
    } else {
        return Err(VmError::TypeError);
    };
    ctx.set_return(result);
    Ok(())
}

pub fn sin(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.sin());
    Ok(())
}

pub fn cos(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.cos());
    Ok(())
}

pub fn sqrt(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.sqrt());
    Ok(())
}

pub fn floor(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.floor());
    Ok(())
}

pub fn ceil(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.ceil());
    Ok(())
}

pub fn round(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.round());
    Ok(())
}

pub fn pow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let base = ctx.arg(0)?;
    let exp = ctx.arg(1)?;
    let result = match (base.as_f64(), exp.as_f64()) {
        (Some(b), Some(e)) => TaggedValue::from_f64(b.powf(e)),
        _ => match (base.as_i64(), exp.as_i64()) {
            (Some(b), Some(e)) if e >= 0 => TaggedValue::from_i64(b.wrapping_pow(e as u32)),
            _ => return Err(VmError::TypeError),
        },
    };
    ctx.set_return(result);
    Ok(())
}

pub fn log(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.ln());
    Ok(())
}
