//! Engine initialization — sets up the TypePool with intrinsic types,
//! creates the VM, and prepares the runtime environment.

use std::sync::Arc;

use gc::GcConfig;
pub use interpreter::{JitConfig, JitMode, JitStats};
use interpreter::{Vm, VmTypePoolRef};
use stack_pool::StackPool;
use type_pool::TypePool;

pub mod builtin_fns;

// ---------------------------------------------------------------------------
// EngineConfig — configuration for engine startup
// ---------------------------------------------------------------------------

/// Configuration for initializing the Nessa engine.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// GC configuration.
    pub gc: GcConfig,
    /// Tiered JIT configuration.
    pub jit: JitConfig,
    /// Whether to enable debug instrumentation.
    pub debug: bool,
}

// ---------------------------------------------------------------------------
// Engine — the initialized runtime
// ---------------------------------------------------------------------------

/// An initialized Nessa engine ready to run code.
pub struct Engine {
    pub vm: Vm,
    pub config: EngineConfig,
    pub stack_pool: Arc<StackPool>,
}

impl Engine {
    /// Initialize a new engine with the given configuration and default hardware worker pool.
    pub fn new(config: EngineConfig) -> Self {
        Self::new_with_workers(config, Vm::default_worker_threads())
    }

    /// Initialize a new engine with an explicit number of OS worker threads (`>= 1`).
    pub fn new_with_workers(config: EngineConfig, worker_threads: usize) -> Self {
        // Create TypePool with all intrinsic types pre-registered.
        let type_pool = TypePool::with_intrinsics();

        // Create the shared task stack pool.
        let stack_pool = Arc::new(StackPool::new());

        // Install the SIGSEGV handler for guard page detection across live pools.
        stack_pool::install_guard_page_handler();

        // Create the VM with MMTk-backed GC, shared stack pool, and configured worker count.
        let mut vm = Vm::new_with_workers(
            type_pool,
            &config.gc,
            Arc::clone(&stack_pool),
            worker_threads.max(1),
        );
        vm.set_jit_config(config.jit);

        // Register all native builtins (fn pointers + type catalog names).
        builtin_fns::register_all(&mut vm);

        Self {
            vm,
            config,
            stack_pool,
        }
    }

    /// Initialize with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(EngineConfig::default())
    }

    /// Get a read guard to the type pool.
    pub fn type_pool(&self) -> VmTypePoolRef<'_> {
        self.vm.type_pool()
    }

    /// Get a mutable reference to the VM.
    pub fn vm_mut(&mut self) -> &mut Vm {
        &mut self.vm
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use type_pool::Intrinsic;

    #[test]
    fn engine_initializes() {
        let engine = Engine::with_defaults();
        // TypePool should have intrinsics registered.
        let tp = engine.type_pool();
        // intrinsic() returns TypeIndex; these should be valid (non-INVALID).
        let i64_idx = tp.intrinsic(Intrinsic::I64);
        assert_ne!(i64_idx, type_pool::TypeIndex::INVALID);
        let bool_idx = tp.intrinsic(Intrinsic::Bool);
        assert_ne!(bool_idx, type_pool::TypeIndex::INVALID);
        let str_idx = tp.intrinsic(Intrinsic::Str);
        assert_ne!(str_idx, type_pool::TypeIndex::INVALID);
        let unit_idx = tp.intrinsic(Intrinsic::Unit);
        assert_ne!(unit_idx, type_pool::TypeIndex::INVALID);
    }

    #[test]
    fn engine_custom_config() {
        let config = EngineConfig {
            gc: gc::GcConfig {
                heap_size: 1024 * 1024,
                ..Default::default()
            },
            jit: JitConfig::with_mode(JitMode::Off),
            debug: true,
        };
        let engine = Engine::new(config.clone());
        assert_eq!(engine.config.gc.heap_size, 1024 * 1024);
        assert_eq!(engine.vm.jit_config().mode, JitMode::Off);
        assert!(engine.config.debug);
    }
}
