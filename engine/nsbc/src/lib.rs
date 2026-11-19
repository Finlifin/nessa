//! NSBC — Nessa Serialized ByteCode.
//!
//! This crate owns all compilation artifact definitions:
//! - Instruction set (opcodes, instruction encoding, registers)
//! - Compilation output types (`CompiledFunction`, `CodegenOutput`)
//! - Archive format types (file header, section table)
//!
//! Serialization and deserialization live in the companion `nsbc_io` crate.

pub mod archive;
mod display_metadata;
mod function_abi;
pub mod instruction;
mod method_context;
pub mod modular;
pub mod validation;

pub use display_metadata::validate_display_owner;
pub use function_abi::{CaptureAbi, FunctionAbi, ParameterAbi};
pub use modular::{
    ArtifactExports, ArtifactImports, ExportTarget, ExportedRuntimeSymbol, ImportedFunction,
    ImportedGlobal, ImportedModuleRef, ImportedPackageRef, InterfaceModuleStub, ModularArtifact,
    ModuleDescriptor, SymbolLinkSelector,
};
pub use validation::{
    ArtifactError, builtin_references, validate_artifact, validate_modular_artifact,
};

// Re-export instruction-level types at crate root for convenience.
pub use crate::instruction::{
    AddrMode, Constant, Format, FuncId, Instruction, InstructionData, Opcode, Reg,
};

// Re-export archive format types at crate root.
pub use crate::archive::{
    FILE_HEADER_SIZE, FileHeader, MAGIC, SECTION_ENTRY_SIZE, SectionEntry, SectionKind, VERSION,
};

// ---------------------------------------------------------------------------
// CodegenOutput — the result of code generation
// ---------------------------------------------------------------------------

/// A compiled function ready for the runtime bytecode store.
#[derive(Debug)]
pub struct CompiledFunction {
    /// Authenticated compiler-generated Display traversal; ordinary functions have None.
    pub display_owner: Option<type_pool::TypeIndex>,
    pub func_id: FuncId,
    pub name: str_interner::StrId,
    pub instructions: Vec<u32>,
    pub register_count: u8,
    pub param_count: u8,
    pub is_closure: bool,
    pub function_type: type_pool::TypeIndex,
    /// Mandatory physical entry layout separating captures and parameters.
    pub abi: FunctionAbi,
    pub safepoint_pcs: Vec<u32>,
}

/// Output of compiling an entire module.
#[derive(Debug)]
pub struct CodegenOutput {
    pub functions: Vec<CompiledFunction>,
    pub constants: Vec<Constant>,
    pub globals: Vec<GlobalInfo>,
    /// Complete lexical contexts for every instruction selected by [`instruction_requires_scope`].
    pub method_call_scopes: Vec<MethodCallScope>,
}

/// Shared contract across code generation, validation, archive loading, and VM installation
/// identifying which instructions require an entry in [`CodegenOutput::method_call_scopes`].
pub const fn instruction_requires_scope(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::CallIndirect
            | Opcode::CallMethod
            | Opcode::CallMethodFar
            | Opcode::CallIndirectProof
            | Opcode::TraitProof
            | Opcode::TraitCall
            | Opcode::TypeCheck
            | Opcode::TypeCast
            | Opcode::TypeCastSafe
            | Opcode::TypeAssert
            | Opcode::TraitAssert
            | Opcode::StoreGlobal
            | Opcode::StoreGlobalWide
            | Opcode::LoadField
            | Opcode::StoreField
            | Opcode::NewEnum
            | Opcode::EnumField
            | Opcode::ErrorOk
            | Opcode::ErrorErr
            | Opcode::ErrorIsOk
            | Opcode::ErrorPayload
    )
}

/// Lexical context of an emitted instruction covered by [`instruction_requires_scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodCallScope {
    pub func_id: FuncId,
    pub pc: u32,
    pub scope: u32,
}

/// Index of a global binding shared by every function in an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalId(pub u32);

/// Runtime schema for a module binding; its first store initializes its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalInfo {
    pub type_index: type_pool::TypeIndex,
    pub is_mutable: bool,
}

/// A self-contained compilation unit with its exact-index runtime metadata.
pub struct CompiledArtifact {
    pub codegen_output: CodegenOutput,
    pub type_pool: type_pool::TypePool,
    /// Saved executable entry, which may be a generated module startup function.
    pub entry: Option<FuncId>,
    pub builtin_abi_version: u32,
    pub builtins: Vec<BuiltinImport>,
    /// Embedded WebAssembly modules (`SectionKind::Wasm`) and their typed bindings.
    pub wasm_modules: Vec<WasmModuleDef>,
}

impl std::fmt::Debug for CompiledArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompiledArtifact")
            .field("codegen_output", &self.codegen_output)
            .field("type_count", &self.type_pool.len())
            .field("entry", &self.entry)
            .field("builtin_abi_version", &self.builtin_abi_version)
            .field("builtins", &self.builtins)
            .field("wasm_modules", &self.wasm_modules.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinImport {
    pub id: u32,
    pub name: String,
}

/// Typed Nessa <-> WebAssembly export binding inside an embedded [`WasmModuleDef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmBindingDef {
    pub nessa_name: String,
    pub wasm_export: String,
    pub signature: type_pool::TypeIndex,
}

/// Embedded WebAssembly module stored in `SectionKind::Wasm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmModuleDef {
    pub name: String,
    pub wasm_bytes: Vec<u8>,
    pub bindings: Vec<WasmBindingDef>,
}
