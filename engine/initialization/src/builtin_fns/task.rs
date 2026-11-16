//! Task scheduling and `Future` combinator builtins (`638..=642`).

use interpreter::{BuiltinCtx, NativeValue, VmError};

/// `638` `__task_yield() -> Unit`
pub fn task_yield(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.task_yield();
    Ok(())
}

/// `639` `__task_await_all(futures: List) -> List`
pub fn task_await_all(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg_rooted(0)?;
    ctx.task_await_all(&list)
}

/// `640` `__task_await_any(futures: List) -> Any`
pub fn task_await_any(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg_rooted(0)?;
    ctx.task_await_any(&list)
}

/// `641` `__task_is_ready(future: Future) -> bool`
pub fn task_is_ready(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let root = ctx.arg_rooted(0)?;
    let val = ctx.load_rooted(&root)?;
    let ready = ctx.task_is_ready(val)?;
    ctx.return_native(NativeValue::Bool(ready))
}

/// `642` `__task_await_indexed(futures: List) -> (i64, Any)`
pub fn task_await_indexed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg_rooted(0)?;
    ctx.task_await_indexed(&list)
}

/// `643` `__host_call(op: String, args: List) -> (bool, Any)`
pub fn host_call(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let op = ctx.arg_string(0)?;
    let args = ctx.arg_rooted(1)?;
    ctx.invoke_host_call(&op, &args)
}
