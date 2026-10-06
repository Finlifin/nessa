//! Engine initialization — sets up the TypePool with intrinsic types,
//! creates the VM, and prepares the runtime environment.

use gc::GcConfig;
use interpreter::Vm;
use stack_pool::StackPool;
use std::sync::Arc;
use type_pool::TypePool;

pub mod builtin_fns;

// ---------------------------------------------------------------------------
// EngineConfig — configuration for engine startup
// ---------------------------------------------------------------------------

/// Configuration for initializing the Nessa engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// GC configuration.
    pub gc: GcConfig,
    /// Whether to enable debug instrumentation.
    pub debug: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            gc: GcConfig::default(),
            debug: false,
        }
    }
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
    /// Initialize a new engine with the given configuration.
    pub fn new(config: EngineConfig) -> Self {
        // Create TypePool with all intrinsic types pre-registered.
        let type_pool = TypePool::with_intrinsics();

        // Create the shared task stack pool.
        let stack_pool = Arc::new(StackPool::new());

        // Install the SIGSEGV handler for guard page detection.
        // Safety: the leaked Arc ensures the pool lives for the program's lifetime.
        unsafe {
            let static_ref: &'static StackPool = &*Arc::into_raw(Arc::clone(&stack_pool));
            stack_pool::install_guard_page_handler(static_ref);
        }

        // Create the VM with MMTk-backed GC and shared stack pool.
        let mut vm = Vm::new(type_pool, &config.gc, Arc::clone(&stack_pool));

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

    /// Get a reference to the type pool.
    pub fn type_pool(&self) -> &TypePool {
        &self.vm.type_pool
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
            debug: true,
        };
        let engine = Engine::new(config.clone());
        assert_eq!(engine.config.gc.heap_size, 1024 * 1024);
        assert!(engine.config.debug);
    }
}
