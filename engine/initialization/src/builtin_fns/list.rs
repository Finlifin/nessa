//! List builtins (skeleton).

use interpreter::{BuiltinCtx, VmError};
use runtime::TaggedValue;

/// Allocate an empty list object.
///
/// Layout is not fully specified yet; returns unit until NewList / list
/// heap layout is wired. Callers in std should treat this as experimental.
pub fn list_init(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    // Placeholder: empty list representation pending heap layout.
    ctx.set_return(TaggedValue::UNIT);
    Ok(())
}
