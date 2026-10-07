//! NSBC — Nessa Serialized ByteCode.
//!
//! This crate owns all compilation artifact definitions:
//! - Instruction set (opcodes, instruction encoding, registers)
//! - Compilation output types (`CompiledFunction`, `CodegenOutput`)
//! - Archive format types (file header, section table)
//! - In-memory bytecode store (`BytecodeStore`)
//!
//! Serialization and deserialization live in the companion `nsbc_io` crate.

pub mod archive;
mod display_metadata;
mod function_abi;
pub mod instruction;
mod method_context;
pub mod validation;

pub use display_metadata::validate_display_owner;
pub use function_abi::{CaptureAbi, FunctionAbi, ParameterAbi};
pub use validation::{ArtifactError, builtin_references, uses_error_envelopes, validate_artifact};

// Re-export instruction-level types at crate root for convenience.
pub use crate::instruction::{
    AddrMode, Constant, Format, FuncHeader, FuncId, Instruction, InstructionData, Opcode, Reg,
};

// Re-export archive format types at crate root.
pub use crate::archive::{
    FILE_HEADER_SIZE, FileHeader, MAGIC, SECTION_ENTRY_SIZE, SectionEntry, SectionKind, VERSION,
};

// ---------------------------------------------------------------------------
// CodegenOutput — the result of code generation
// ---------------------------------------------------------------------------

/// A compiled function ready for the bytecode store.
#[derive(Debug)]
pub struct CompiledFunction {
    /// Authenticated compiler-generated Display traversal; legacy functions have None.
    pub display_owner: Option<type_pool::TypeIndex>,
    pub func_id: FuncId,
    pub name: str_interner::StrId,
    pub instructions: Vec<u32>,
    pub register_count: u8,
    pub param_count: u8,
    pub is_closure: bool,
    pub function_type: type_pool::TypeIndex,
    /// None preserves the original entry interpretation of legacy bytecode.
    pub abi: Option<FunctionAbi>,
    pub safepoint_pcs: Vec<u32>,
}

/// Output of compiling an entire module.
#[derive(Debug)]
pub struct CodegenOutput {
    pub scope_coverage: ScopeCoverage,
    pub functions: Vec<CompiledFunction>,
    pub constants: Vec<Constant>,
    pub globals: Vec<GlobalInfo>,
    /// Complete lexical contexts for the instructions selected by scope_coverage.
    /// None denotes legacy or host metadata.
    pub method_call_scopes: Option<Vec<MethodCallScope>>,
}

/// Which instructions require lexical entries in the scope table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeCoverage {
    Calls,
    CallsAndTypes,
}

impl ScopeCoverage {
    /// The opcode contract is shared by generation, loading and runtime installation.
    pub fn covers(self, opcode: Opcode) -> bool {
        matches!(
            opcode,
            Opcode::CallIndirect
                | Opcode::CallMethod
                | Opcode::CallMethodFar
                | Opcode::CallIndirectProof
                | Opcode::TraitProof
                | Opcode::TraitCall
        ) || (self == Self::CallsAndTypes
            && matches!(
                opcode,
                Opcode::TypeCheck
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
            ))
    }
}

/// Lexical context of an emitted instruction covered by ScopeCoverage.
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
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinImport {
    pub id: u32,
    pub name: String,
}

// ---------------------------------------------------------------------------
// BytecodeStore
// ---------------------------------------------------------------------------

/// Runtime bytecode store — holds all loaded functions and the constant pool.
#[derive(Debug)]
pub struct BytecodeStore {
    /// Function headers, indexed by FuncId.
    headers: Vec<FuncHeader>,
    /// Raw 32-bit instruction words per function, indexed by FuncId.
    code: Vec<Vec<u32>>,
    /// Global constant pool.
    constants: Vec<Constant>,
    /// Schema retained with the artifact for its runtime global table.
    globals: Vec<GlobalInfo>,
}

impl BytecodeStore {
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            code: Vec::new(),
            constants: Vec::new(),
            globals: Vec::new(),
        }
    }

    /// Load all functions from a codegen output.
    pub fn load_codegen_output(&mut self, output: CodegenOutput) {
        for func in output.functions {
            self.load_function(func);
        }
        self.constants.extend(output.constants);
        self.globals.extend(output.globals);
    }

    /// Load a single compiled function.
    pub fn load_function(&mut self, func: CompiledFunction) {
        let id = func.func_id.0 as usize;
        // Grow storage if needed.
        while self.headers.len() <= id {
            self.headers.push(FuncHeader {
                func_id: FuncId(self.headers.len() as u32),
                register_count: 0,
                param_count: 0,
                has_variadic: false,
                is_closure: false,
            });
            self.code.push(Vec::new());
        }
        self.headers[id] = FuncHeader {
            func_id: func.func_id,
            register_count: func.register_count,
            param_count: func.param_count,
            has_variadic: false,
            is_closure: func.is_closure,
        };
        self.code[id] = func.instructions;
    }

    /// Get the header for a function.
    pub fn header(&self, id: FuncId) -> Option<&FuncHeader> {
        self.headers.get(id.0 as usize)
    }

    /// Get the instruction words for a function.
    pub fn instructions(&self, id: FuncId) -> Option<&[u32]> {
        self.code.get(id.0 as usize).map(|v| v.as_slice())
    }

    /// Fetch a single instruction at an offset within a function.
    pub fn fetch(&self, id: FuncId, pc: usize) -> Option<Instruction> {
        let code = self.code.get(id.0 as usize)?;
        let word = code.get(pc)?;
        Instruction::decode(*word)
    }

    /// Number of loaded functions.
    pub fn function_count(&self) -> usize {
        self.headers.len()
    }

    pub fn globals(&self) -> &[GlobalInfo] {
        &self.globals
    }

    /// Get a constant by index.
    pub fn constant(&self, idx: u32) -> Option<&Constant> {
        self.constants.get(idx as usize)
    }

    /// Number of constants.
    pub fn constant_count(&self) -> usize {
        self.constants.len()
    }
}

impl Default for BytecodeStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_fetch() {
        let mut store = BytecodeStore::new();
        let nop = Instruction::nop().encode();
        let ret = Instruction::return_unit().encode();
        store.load_function(CompiledFunction {
            display_owner: None,
            func_id: FuncId(0),
            name: str_interner::StrId::from_raw(0),
            instructions: vec![nop, ret],
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
            abi: None,
            safepoint_pcs: vec![],
        });

        assert_eq!(store.function_count(), 1);
        let header = store.header(FuncId(0)).unwrap();
        assert_eq!(header.register_count, 1);

        let instr = store.fetch(FuncId(0), 0).unwrap();
        assert_eq!(instr, Instruction::nop());

        let instr = store.fetch(FuncId(0), 1).unwrap();
        assert_eq!(instr, Instruction::return_unit());
    }

    #[test]
    fn load_codegen_output_with_constants() {
        let mut store = BytecodeStore::new();
        let global = GlobalInfo {
            type_index: type_pool::Intrinsic::I64.type_index(),
            is_mutable: false,
        };
        store.load_codegen_output(CodegenOutput {
            scope_coverage: crate::ScopeCoverage::Calls,
            method_call_scopes: None,
            globals: vec![global],
            functions: vec![CompiledFunction {
                display_owner: None,
                func_id: FuncId(0),
                name: str_interner::StrId::from_raw(0),
                instructions: vec![Instruction::nop().encode()],
                register_count: 0,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
                abi: None,
                safepoint_pcs: vec![],
            }],
            constants: vec![Constant::Int(42), Constant::Str("hello".into())],
        });

        assert_eq!(store.function_count(), 1);
        assert_eq!(store.constant_count(), 2);
        assert_eq!(store.globals(), &[global]);
        assert!(matches!(store.constant(0), Some(Constant::Int(42))));
    }

    #[test]
    fn sparse_func_ids() {
        let mut store = BytecodeStore::new();
        store.load_function(CompiledFunction {
            display_owner: None,
            func_id: FuncId(5),
            name: str_interner::StrId::from_raw(0),
            instructions: vec![Instruction::nop().encode()],
            register_count: 2,
            param_count: 1,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
            abi: None,
            safepoint_pcs: vec![],
        });

        assert_eq!(store.function_count(), 6); // 0..5 + the func at 5
        assert!(store.fetch(FuncId(0), 0).is_none()); // slot 0 has empty code
        let instr = store.fetch(FuncId(5), 0).unwrap();
        assert_eq!(instr, Instruction::nop());
    }
}
