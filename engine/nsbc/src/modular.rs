//! Modular `.nsbc` archive descriptors for `SectionKind::Imports` and `SectionKind::Exports`.
//!
//! Enables separate per-package `.nsbc` compilation (using exported metadata & interface stubs
//! directly from `.nsbc` without dependency `.ns` sources) and per-`mod` on-demand VM loading,
//! linking, bytecode relocation, and dependency-ordered `__init__` execution.

use type_pool::{TypeId, TypeIndex};

use crate::{BuiltinImport, CodegenOutput, FuncId, FunctionAbi, GlobalId, WasmModuleDef};

/// Identifies a linkable runtime function, method, or shared value across `.nsbc` archives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolLinkSelector {
    /// Module-level or nominal-type-associated binding (`module_path`, optional `owner_type_id`, `name`).
    ScopedSymbol {
        module_path: Vec<String>,
        owner_type_id: Option<TypeId>,
        name: String,
    },
    /// Inherent `impl` method on a type (`receiver_type_id`, `name`).
    InherentMethod {
        receiver_type_id: TypeId,
        name: String,
    },
    /// Concrete or derived `impl Trait for Type` method (`trait_type_id`, `implementor_type_id`, `name`).
    TraitMethod {
        trait_type_id: TypeId,
        implementor_type_id: TypeId,
        name: String,
    },
    /// Compiler-synthesized `derive T: Json` static decoder (`from_json`).
    DerivedJsonFrom { implementor_type_id: TypeId },
    /// Compiler-synthesized `derive T: Json` internal recursive stream helper (`__json_write` / `__json_read`).
    DerivedJsonHelper {
        implementor_type_id: TypeId,
        is_write: bool,
    },
    /// Nested local helper function instantiated inside a specialized `TraitDeriveFn` / trait default adapter.
    DefaultTraitHelper {
        trait_type_id: TypeId,
        implementor_type_id: TypeId,
        method_name: String,
        nested_name: String,
    },
}

/// Direct or transitive external package referenced by a [`ModularArtifact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedPackageRef {
    pub qualified_name: String,
    pub version: String,
    pub identity: [u8; 16],
    /// Deterministic SHA-256 digest of the dependency's exported public interface.
    pub interface_hash: [u8; 32],
}

/// External function imported into `FuncId(codegen_output.functions.len() + index)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFunction {
    pub package_index: u32,
    pub selector: SymbolLinkSelector,
    pub function_type: TypeIndex,
    pub param_count: u8,
    pub abi: FunctionAbi,
    pub display_owner: Option<TypeIndex>,
}

/// External `pub global` or `pub const` imported into `GlobalId(codegen_output.globals.len() + index)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedGlobal {
    pub package_index: u32,
    pub module_path: Vec<String>,
    pub owner_type_id: Option<TypeId>,
    pub name: String,
    pub type_index: TypeIndex,
    pub is_mutable: bool,
}

/// External module (`mod`) referenced by one or more local [`ModuleDescriptor`]s.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportedModuleRef {
    pub package_index: u32,
    pub module_path: Vec<String>,
}

/// Payload of `SectionKind::Imports` (`5`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactImports {
    pub packages: Vec<ImportedPackageRef>,
    pub functions: Vec<ImportedFunction>,
    pub globals: Vec<ImportedGlobal>,
    pub module_dependencies: Vec<ImportedModuleRef>,
}

/// Per-`mod` compilation, loading, and initialization unit inside [`ArtifactExports`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDescriptor {
    /// Logical path inside the package (`[]` for root `lib.ns`/`main.ns`, `["io"]`, `["net", "http"]`).
    pub module_path: Vec<String>,
    /// Stable 128-bit `TypeId` of this `TypeKind::Module` when backed by a nominal module type.
    pub module_type_id: Option<TypeId>,
    /// Local `FuncId`s (`0 .. codegen_output.functions.len()`) owned by this module.
    pub owned_functions: Vec<FuncId>,
    /// Local `GlobalId`s (`0 .. codegen_output.globals.len()`) owned by this module.
    pub owned_globals: Vec<GlobalId>,
    /// Ordered per-scope initializer `FuncId`s within this module (`__init__` & top-level bindings).
    pub scope_initializers: Vec<FuncId>,
    /// Indices into `ArtifactExports::modules` referenced by functions/types in this module.
    pub local_module_deps: Vec<u32>,
    /// Indices into `ArtifactExports::modules` whose initializers must run before `scope_initializers`.
    pub local_init_prereqs: Vec<u32>,
    /// Indices into `ArtifactImports::module_dependencies` referenced by functions/types in this module.
    pub external_module_deps: Vec<u32>,
    /// Indices into `ArtifactImports::module_dependencies` whose initializers must run before `scope_initializers`.
    pub external_init_prereqs: Vec<u32>,
    /// Optional `exe` entry `main` `FuncId` on the package root module.
    pub main_func: Option<FuncId>,
}

/// Resolved target of an [`ExportedRuntimeSymbol`] inside the defining package's local tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportTarget {
    Function {
        func_id: FuncId,
        function_type: TypeIndex,
        param_count: u8,
        abi: FunctionAbi,
        display_owner: Option<TypeIndex>,
    },
    Global {
        global_id: GlobalId,
        type_index: TypeIndex,
        is_mutable: bool,
    },
    Type {
        type_index: TypeIndex,
        type_id: TypeId,
    },
}

/// Exported linkable symbol in `SectionKind::Exports`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedRuntimeSymbol {
    pub selector: SymbolLinkSelector,
    /// Index into `ArtifactExports::modules` identifying the owning `mod` unit.
    pub owner_module_index: u32,
    pub target: ExportTarget,
}

/// Synthesized declaration-only Nessa interface stub for one module in `SectionKind::Exports`,
/// allowing downstream packages to resolve types, traits, effects, named/default parameters,
/// and `derive fn` templates directly from `.nsbc` without reading `.ns` source files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceModuleStub {
    pub module_path: Vec<String>,
    pub stub_source: String,
}

/// Payload of `SectionKind::Exports` (`6`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactExports {
    pub qualified_name: String,
    pub version: String,
    pub identity: [u8; 16],
    /// Deterministic SHA-256 of the exported public interface (unchanged by private body edits).
    pub interface_hash: [u8; 32],
    /// Canonical `package.toml` document text for dependency graph & Merkle identity verification.
    pub manifest_toml: String,
    /// Whether this package carries standard-library `'builtin` / refinement privileges.
    pub is_privileged_std: bool,
    /// Per-`mod` descriptors enabling fine-grained module loading & `__init__` execution.
    pub modules: Vec<ModuleDescriptor>,
    /// Linkable runtime symbols exported by this package.
    pub symbols: Vec<ExportedRuntimeSymbol>,
    /// Bodyless compile-time interface stubs for root (`[]`) and submodules.
    pub interface_modules: Vec<InterfaceModuleStub>,
}

/// A modular `.nsbc` compilation unit carrying local bytecode plus `IMPORTS` and `EXPORTS` sections.
pub struct ModularArtifact {
    pub codegen_output: CodegenOutput,
    pub type_pool: type_pool::TypePool,
    pub builtin_abi_version: u32,
    pub builtins: Vec<BuiltinImport>,
    pub wasm_modules: Vec<WasmModuleDef>,
    pub imports: ArtifactImports,
    pub exports: ArtifactExports,
}

impl std::fmt::Debug for ModularArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModularArtifact")
            .field("package", &self.exports.qualified_name)
            .field("version", &self.exports.version)
            .field("modules", &self.exports.modules.len())
            .field("local_functions", &self.codegen_output.functions.len())
            .field("imported_functions", &self.imports.functions.len())
            .field("local_globals", &self.codegen_output.globals.len())
            .field("imported_globals", &self.imports.globals.len())
            .field("type_count", &self.type_pool.len())
            .field("builtin_abi_version", &self.builtin_abi_version)
            .field("wasm_modules", &self.wasm_modules.len())
            .finish()
    }
}
