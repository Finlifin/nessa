//! Math builtins.

use interpreter::{BuiltinCtx, VmError};
use nsbc::Opcode;
use runtime::Number;

pub fn abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let result = match ctx.arg_number(0)? {
        Number::I64(value) => match value.checked_abs() {
            Some(value) => Number::I64(value),
            None => Number::I128(-(value as i128)),
        },
        Number::I128(value) => Number::I128(value.checked_abs().ok_or(VmError::NumericOverflow)?),
        Number::U64(value) => Number::U64(value),
        Number::U128(value) => Number::U128(value),
        Number::F64(value) => Number::F64(value.abs()),
    };
    ctx.return_number(result)
}

pub fn sin(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.sin())?;
    Ok(())
}

pub fn cos(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.cos())?;
    Ok(())
}

pub fn sqrt(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.sqrt())?;
    Ok(())
}

pub fn floor(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.floor())?;
    Ok(())
}

pub fn ceil(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.ceil())?;
    Ok(())
}

pub fn round(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.round())?;
    Ok(())
}

pub fn pow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let base = ctx.arg_number(0)?;
    let exponent = ctx.arg_number(1)?;
    if matches!(base, Number::F64(_)) || matches!(exponent, Number::F64(_)) {
        return ctx.return_f64(base.to_f64().powf(exponent.to_f64()));
    }
    let mut exponent = exponent.to_u64_checked().ok_or(VmError::NumericOverflow)?;
    let mut factor = base;
    let mut result = match base {
        Number::U64(_) => Number::U64(1),
        Number::U128(_) => Number::U128(1),
        Number::I128(_) => Number::I128(1),
        _ => Number::I64(1),
    };
    // Exponentiation by squaring bounds work by exponent bit width and uses
    // the same checked arithmetic and promotion rules as VM multiplication.
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = result.binary(factor, Opcode::Mul)?;
        }
        exponent >>= 1;
        if exponent != 0 {
            factor = factor.binary(factor, Opcode::Mul)?;
        }
    }
    ctx.return_number(result)
}

pub fn log(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.ln())?;
    Ok(())
}
