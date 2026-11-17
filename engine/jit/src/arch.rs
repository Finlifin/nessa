//! Target architecture abstraction for Nessa's JIT backend.
//!
//! `x86_64` Unix hosts compile and execute native machine code directly.
//! Non-`x86_64` targets (`aarch64`, etc.) provide clean trait-level fallbacks so
//! the workspace builds warning-free everywhere and runs deterministically in the interpreter.

use crate::code_cache::JitError;
use crate::{CompilationInput, EmittedCode};

/// Whether the current host target supports native JIT execution.
#[inline]
pub const fn is_target_supported() -> bool {
    cfg!(all(target_arch = "x86_64", any(unix, windows)))
}

#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub fn compile_tier1(input: &CompilationInput<'_>) -> Result<EmittedCode, JitError> {
    crate::tier1::compile_function_tier1(input)
}

#[cfg(not(all(target_arch = "x86_64", any(unix, windows))))]
pub fn compile_tier1(_input: &CompilationInput<'_>) -> Result<EmittedCode, JitError> {
    Err(JitError::UnsupportedTarget)
}

#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub fn compile_tier2(input: &CompilationInput<'_>) -> Result<EmittedCode, JitError> {
    crate::tier2::compile_function_tier2(input)
}

#[cfg(not(all(target_arch = "x86_64", any(unix, windows))))]
pub fn compile_tier2(_input: &CompilationInput<'_>) -> Result<EmittedCode, JitError> {
    Err(JitError::UnsupportedTarget)
}
