//! I/O builtins: print, println.

use interpreter::{BuiltinCtx, VmError};
use std::io::Write;

pub fn print(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    let formatted = ctx.format_value(val);
    let _ = write!(std::io::stdout(), "{formatted}");
    let _ = std::io::stdout().flush();
    ctx.return_unit();
    Ok(())
}

pub fn println(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let val = ctx.arg(0)?;
    println!("{}", ctx.format_value(val));
    ctx.return_unit();
    Ok(())
}
