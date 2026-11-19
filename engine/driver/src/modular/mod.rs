//! Modular per-package `.nsbc` compilation & per-`mod` dynamic VM loading.

pub mod compiler;
pub mod interface;
pub mod loader;

pub use compiler::{ModularCompilationOutput, load_cached_modular_artifact};
pub use loader::{ModuleLoadState, ModuleLoader, PreparedModuleLoad};
