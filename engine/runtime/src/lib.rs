use nsbc::{FuncId, GlobalInfo, Reg};
use type_pool::TypeIndex;

pub mod stacks;
pub use stacks::{ContinuationId, PromptId, StackContext, StackError, TaskStacks};
pub mod numeric;
pub use numeric::{Number, NumericError};

pub mod builtin;
pub use builtin::ids;
pub use builtin::{
    BUILTIN_ABI_VERSION, BUILTIN_FN_META, BuiltinCatalog, BuiltinFnId, BuiltinFnMeta, BuiltinKind,
    catalog_lookup, catalog_register_intrinsic_types, catalog_register_type, lookup_builtin_fn_id,
    lookup_builtin_fn_meta,
};

// ---------------------------------------------------------------------------
// TaggedValue — the universal 64-bit runtime value
// ---------------------------------------------------------------------------

/// A 64-bit tagged value.  The low 3 bits encode the tag.
///
/// ```text
/// [Payload 61 bits][Tag 3 bits]
///   000  HeapObject   — payload is 8-byte-aligned pointer
///   001  Immediate    — payload encodes an inline value
///   01x  Reserved
///   1xx  Reserved
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct TaggedValue(u64);

// Tag constants
const TAG_MASK: u64 = 0b111;
const TAG_HEAP: u64 = 0b000;
const TAG_IMM: u64 = 0b001;

// Immediate sub-tag (bits [6:3] of the value, i.e. bits [3:0] of payload)
const IMM_SUB_SHIFT: u32 = 3;
const IMM_SUB_MASK: u64 = 0xF << IMM_SUB_SHIFT;
const IMM_DATA_SHIFT: u32 = 7; // payload data starts at bit 7

const IMM_I_SMALL: u64 = 0x0;
const IMM_U_SMALL: u64 = 0x1;
const IMM_F64: u64 = 0x2;
const IMM_BOOL: u64 = 0x3;
const IMM_SYMBOL: u64 = 0x4;
const IMM_NULL: u64 = 0x5;
const IMM_UNIT: u64 = 0x6;
const IMM_CHAR: u64 = 0x7;
const IMM_TYPE: u64 = 0x8;
const IMM_ENUM: u64 = 0x9;
const IMM_TRAIT_PROOF: u64 = 0xA;

impl TaggedValue {
    pub const UNIT: Self = Self::make_imm(IMM_UNIT, 0);
    pub const NULL: Self = Self::make_imm(IMM_NULL, 0);
    pub const TRUE: Self = Self::make_imm(IMM_BOOL, 1);
    pub const FALSE: Self = Self::make_imm(IMM_BOOL, 0);

    const fn make_imm(sub: u64, data: u64) -> Self {
        Self(TAG_IMM | (sub << IMM_SUB_SHIFT) | (data << IMM_DATA_SHIFT))
    }

    // -- Constructors --------------------------------------------------------

    /// Encode an immediate integer, or return None when heap storage is needed.
    pub fn try_from_i64(v: i64) -> Option<Self> {
        (-(1_i64 << 56)..(1_i64 << 56))
            .contains(&v)
            .then(|| Self::make_imm(IMM_I_SMALL, (v as u64) & ((1_u64 << 57) - 1)))
    }

    /// Encode an immediate unsigned integer without truncation.
    pub fn try_from_u64(v: u64) -> Option<Self> {
        (v < (1_u64 << 57)).then(|| Self::make_imm(IMM_U_SMALL, v))
    }

    /// Encode a float only when all bits survive immediate compression.
    pub fn try_from_f64(v: f64) -> Option<Self> {
        let bits = v.to_bits();
        (bits & 0x7f == 0).then(|| Self::make_imm(IMM_F64, bits >> 7))
    }

    /// Encode a known small integer. Use the VM numeric factory for arbitrary values.
    ///
    /// # Panics
    /// Panics when `v` needs heap storage.
    pub fn from_i64(v: i64) -> Self {
        Self::try_from_i64(v).expect("integer needs heap storage")
    }

    /// Encode a known small unsigned integer.
    ///
    /// # Panics
    /// Panics when `v` needs heap storage.
    pub fn from_u64(v: u64) -> Self {
        Self::try_from_u64(v).expect("unsigned integer needs heap storage")
    }

    /// Encode a float known to fit the compressed immediate representation exactly.
    ///
    /// # Panics
    /// Panics when any float bits would be lost; use the VM numeric factory instead.
    pub fn from_f64(v: f64) -> Self {
        Self::try_from_f64(v).expect("float needs heap storage")
    }

    pub fn from_bool(v: bool) -> Self {
        if v { Self::TRUE } else { Self::FALSE }
    }

    pub fn from_char(ch: char) -> Self {
        Self::make_imm(IMM_CHAR, ch as u64)
    }

    /// Construct a type value holding a pool-local index, not a stable TypeId.
    /// Pool membership is checked by the VM using its active type pool.
    pub fn from_type(ty: TypeIndex) -> Option<Self> {
        (ty != TypeIndex::INVALID).then(|| Self::make_imm(IMM_TYPE, u64::from(ty.as_u32())))
    }

    /// Encode a pool-local enum identity and a 25-bit variant tag. The VM
    /// checks pool membership and whether this variant has an empty payload.
    pub fn from_enum(ty: TypeIndex, variant: u32) -> Option<Self> {
        (ty != TypeIndex::INVALID && variant < (1 << 25)).then(|| {
            Self::make_imm(
                IMM_ENUM,
                (u64::from(ty.as_u32()) << 25) | u64::from(variant),
            )
        })
    }

    /// Decode enum representation only; legacy symbols are never enum values.
    pub fn as_enum(self) -> Option<(TypeIndex, u32)> {
        if !self.is_immediate() || self.imm_sub() != IMM_ENUM {
            return None;
        }
        let data = self.imm_data();
        let ty = TypeIndex::from_raw((data >> 25) as u32);
        (ty != TypeIndex::INVALID).then_some((ty, (data & ((1 << 25) - 1)) as u32))
    }

    /// Encode an opaque VM proof handle. IDs are globally unique and registry
    /// membership, view and implementor are checked before use.
    pub fn from_trait_proof(id: u64) -> Option<Self> {
        (id != 0 && id < (1 << 57)).then(|| Self::make_imm(IMM_TRAIT_PROOF, id))
    }

    pub fn as_trait_proof(self) -> Option<u64> {
        (self.is_immediate() && self.imm_sub() == IMM_TRAIT_PROOF)
            .then(|| self.imm_data())
            .filter(|&id| id != 0)
    }

    pub fn from_symbol(sym: u64) -> Self {
        Self::make_imm(IMM_SYMBOL, sym)
    }

    /// Create a HeapObject tagged value from a raw object pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid GC-managed object payload.
    pub unsafe fn from_heap_ptr(ptr: *const u8) -> Self {
        debug_assert!((ptr as u64) & TAG_MASK == 0, "heap pointer not aligned");
        Self(ptr as u64 | TAG_HEAP)
    }

    // -- Tag queries ---------------------------------------------------------

    pub const fn tag(self) -> u64 {
        self.0 & TAG_MASK
    }

    pub const fn is_heap(self) -> bool {
        self.tag() == TAG_HEAP && self.0 != 0
    }

    pub const fn is_immediate(self) -> bool {
        self.tag() == TAG_IMM
    }

    fn imm_sub(self) -> u64 {
        (self.0 & IMM_SUB_MASK) >> IMM_SUB_SHIFT
    }

    fn imm_data(self) -> u64 {
        self.0 >> IMM_DATA_SHIFT
    }

    pub fn is_null(self) -> bool {
        self.is_immediate() && self.imm_sub() == IMM_NULL
    }

    pub fn is_unit(self) -> bool {
        self.is_immediate() && self.imm_sub() == IMM_UNIT
    }

    // -- Extractors ----------------------------------------------------------

    pub fn as_i64(self) -> Option<i64> {
        if self.is_immediate() && self.imm_sub() == IMM_I_SMALL {
            let raw = self.imm_data();
            // sign-extend from 57 bits
            let val = if raw & (1 << 56) != 0 {
                (raw | !((1u64 << 57) - 1)) as i64
            } else {
                raw as i64
            };
            Some(val)
        } else {
            None
        }
    }

    pub fn as_u64(self) -> Option<u64> {
        if self.is_immediate() && self.imm_sub() == IMM_U_SMALL {
            Some(self.imm_data())
        } else {
            None
        }
    }

    pub fn as_f64(self) -> Option<f64> {
        if self.is_immediate() && self.imm_sub() == IMM_F64 {
            let bits = self.imm_data() << 7;
            Some(f64::from_bits(bits))
        } else {
            None
        }
    }

    pub fn as_bool(self) -> Option<bool> {
        if self.is_immediate() && self.imm_sub() == IMM_BOOL {
            Some(self.imm_data() != 0)
        } else {
            None
        }
    }

    pub fn as_char(self) -> Option<char> {
        if self.is_immediate() && self.imm_sub() == IMM_CHAR {
            char::from_u32(self.imm_data() as u32)
        } else {
            None
        }
    }

    /// Decode a type value, rejecting malformed raw payloads and INVALID.
    /// This validates the representation; the VM validates pool membership.
    pub fn as_type(self) -> Option<TypeIndex> {
        if !self.is_immediate() || self.imm_sub() != IMM_TYPE {
            return None;
        }
        let raw = u32::try_from(self.imm_data()).ok()?;
        let ty = TypeIndex::from_raw(raw);
        (ty != TypeIndex::INVALID).then_some(ty)
    }

    /// Get the raw heap pointer (payload pointer, after ObjectHeader).
    pub fn as_heap_ptr(self) -> Option<*const u8> {
        if self.is_heap() {
            Some((self.0 & !TAG_MASK) as *const u8)
        } else {
            None
        }
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Reconstruct a TaggedValue from its raw u64 representation.
    pub const fn from_raw(v: u64) -> Self {
        Self(v)
    }
}

impl std::fmt::Debug for TaggedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_null() {
            write!(f, "null")
        } else if self.is_unit() {
            write!(f, "()")
        } else if let Some(v) = self.as_i64() {
            write!(f, "{v}i")
        } else if let Some(v) = self.as_u64() {
            write!(f, "{v}u")
        } else if let Some(v) = self.as_bool() {
            write!(f, "{v}")
        } else if let Some(v) = self.as_char() {
            write!(f, "'{v}'")
        } else if let Some(ty) = self.as_type() {
            write!(f, "Type({ty:?})")
        } else if self.is_heap() {
            write!(f, "HeapObj({:#x})", self.0 & !TAG_MASK)
        } else {
            write!(f, "TaggedValue({:#018x})", self.0)
        }
    }
}

// ---------------------------------------------------------------------------
// RegisterFile — the 32 GP registers per task
// ---------------------------------------------------------------------------

pub const GP_REGISTER_COUNT: usize = 32;

/// The register file for a single task / call frame.
#[derive(Clone)]
pub struct RegisterFile {
    pub regs: [TaggedValue; GP_REGISTER_COUNT],
}

impl RegisterFile {
    pub fn new() -> Self {
        Self {
            regs: [TaggedValue::UNIT; GP_REGISTER_COUNT],
        }
    }

    pub fn get(&self, r: Reg) -> TaggedValue {
        self.regs[r.0 as usize]
    }

    pub fn set(&mut self, r: Reg, val: TaggedValue) {
        self.regs[r.0 as usize] = val;
    }
}

impl Default for RegisterFile {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CallFrame — a single frame on the call stack
// ---------------------------------------------------------------------------

/// A frame on the call stack.
#[derive(Clone)]
pub struct CallFrame {
    pub display_state: Option<DisplayState>,
    /// Return address (instruction index to return to).
    pub return_pc: u32,
    /// Function being executed.
    pub func_id: FuncId,
    /// Saved register values (spilled before call).
    pub saved_regs: Vec<(Reg, TaggedValue)>,
    /// Caller-local values, moved into the frame while the callee executes.
    pub local_slots: Vec<TaggedValue>,
    /// Evidence chain for effect handlers.
    pub evidence: Vec<HandlerRef>,
    /// Closure environment pointer (for LoadCapture), None if not a closure call.
    pub closure_env: Option<TaggedValue>,
}

/// Traversal state is owned by a frame/segment; cloning a continuation copies
/// its path independently. Every retained receiver is an enumerated GC root.
#[derive(Clone, Default)]
pub struct DisplayState {
    pub depth: usize,
    pub ancestors: Vec<TaggedValue>,
    pub current_target: bool,
    pub inline_count: usize,
}

/// A reference to an installed effect handler.
#[derive(Debug, Clone, Copy)]
pub struct HandlerRef {
    /// Which effect this handles.
    pub effect_type: TypeIndex,
    /// Index in the handler stack.
    pub handler_idx: u32,
}

// ---------------------------------------------------------------------------
// TaskState — per-task execution state
// ---------------------------------------------------------------------------

/// The status of a task in the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Ready,
    Running,
    Suspended,
    Waiting,
    Finished,
}

/// Unique identifier for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

/// Complete execution state of a single task.
pub struct TaskState {
    pub id: TaskId,
    pub status: TaskStatus,
    /// Active, delimited and captured execution stacks owned by this task.
    pub stacks: TaskStacks,
    /// Parent task (for structured concurrency / task tree).
    pub parent: Option<TaskId>,
    /// Child tasks spawned by this task.
    pub children: Vec<TaskId>,
}

impl TaskState {
    pub fn new(id: TaskId, func_id: FuncId) -> Self {
        Self {
            id,
            status: TaskStatus::Ready,
            stacks: TaskStacks::new(func_id),
            parent: None,
            children: Vec::new(),
        }
    }
}

// Existing register and frame operations always address the active segment.
impl std::ops::Deref for TaskState {
    type Target = StackContext;

    fn deref(&self) -> &Self::Target {
        self.stacks.active()
    }
}

impl std::ops::DerefMut for TaskState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.stacks.active_mut()
    }
}

// ---------------------------------------------------------------------------
// EffectHandler — installed effect handler
// ---------------------------------------------------------------------------

/// An effect handler installed on the handler stack.
#[derive(Debug, Clone)]
pub struct EffectHandler {
    pub effect_type: TypeIndex,
    /// Function to call when the effect is invoked.
    pub handler_func: FuncId,
    /// Whether this is an async handler (spawns child task).
    pub is_async: bool,
    /// Closure and its captures, retained as a GC root while installed.
    pub closure_env: Option<TaggedValue>,
    /// Runtime-provided continuation argument position for capturing handlers.
    pub continuation_param: Option<u8>,
}

// ---------------------------------------------------------------------------
// BytecodeStore — holds all compiled function bytecode
// ---------------------------------------------------------------------------

/// Stores compiled bytecode for all functions in the program.
pub struct BytecodeStore {
    /// Per-function instruction sequences, indexed by FuncId.
    functions: Vec<FunctionCode>,
}

pub struct FunctionCode {
    pub display_owner: Option<type_pool::TypeIndex>,
    pub func_id: FuncId,
    pub instructions: Vec<u32>, // encoded 32-bit instruction words
    pub register_count: u8,
    pub param_count: u8,
    pub is_closure: bool,
    pub function_type: type_pool::TypeIndex,
    /// Explicit entry layout; None retains legacy value-only interpretation.
    pub abi: Option<nsbc::FunctionAbi>,
}

impl BytecodeStore {
    pub fn new() -> Self {
        Self {
            functions: Vec::new(),
        }
    }

    pub fn add_function(&mut self, code: FunctionCode) -> FuncId {
        let id = FuncId(self.functions.len() as u32);
        debug_assert_eq!(code.func_id, id);
        self.functions.push(code);
        id
    }

    pub fn get_function(&self, id: FuncId) -> &FunctionCode {
        &self.functions[id.0 as usize]
    }

    pub fn try_get_function(&self, id: FuncId) -> Option<&FunctionCode> {
        self.functions.get(id.0 as usize)
    }

    pub fn function_count(&self) -> usize {
        self.functions.len()
    }
}

impl Default for BytecodeStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// GlobalTable — runtime global variables
// ---------------------------------------------------------------------------

/// Table of global variables.
pub struct GlobalTable {
    values: Vec<TaggedValue>,
    slots: Vec<GlobalSlot>,
    schema_initialized: bool,
}

struct GlobalSlot {
    info: GlobalInfo,
    initialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalError {
    InvalidIndex(u32),
    Uninitialized(u32),
    Immutable(u32),
    SchemaAlreadyInitialized,
    OutOfMemory,
}

impl GlobalTable {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            slots: Vec::new(),
            schema_initialized: false,
        }
    }

    /// Install a schema before publishing any globals. Values remain unreadable
    /// until their first store, including const bindings.
    pub fn initialize(&mut self, schema: &[GlobalInfo]) -> Result<(), GlobalError> {
        if self.schema_initialized || !self.values.is_empty() {
            return Err(GlobalError::SchemaAlreadyInitialized);
        }
        self.values
            .try_reserve_exact(schema.len())
            .map_err(|_| GlobalError::OutOfMemory)?;
        self.slots
            .try_reserve_exact(schema.len())
            .map_err(|_| GlobalError::OutOfMemory)?;
        self.values.resize(schema.len(), TaggedValue::UNIT);
        self.slots.extend(schema.iter().map(|&info| GlobalSlot {
            info,
            initialized: false,
        }));
        self.schema_initialized = true;
        Ok(())
    }

    pub fn alloc(&mut self, init: TaggedValue) -> u32 {
        let idx = self.values.len() as u32;
        self.values.push(init);
        self.slots.push(GlobalSlot {
            info: GlobalInfo {
                type_index: type_pool::Intrinsic::Any.type_index(),
                is_mutable: true,
            },
            initialized: true,
        });
        idx
    }

    pub fn get(&self, idx: u32) -> Result<TaggedValue, GlobalError> {
        let slot = self
            .slots
            .get(idx as usize)
            .ok_or(GlobalError::InvalidIndex(idx))?;
        if !slot.initialized {
            return Err(GlobalError::Uninitialized(idx));
        }
        Ok(self.values[idx as usize])
    }

    pub fn set(&mut self, idx: u32, val: TaggedValue) -> Result<(), GlobalError> {
        self.check_store(idx)?;
        self.values[idx as usize] = val;
        self.slots[idx as usize].initialized = true;
        Ok(())
    }

    /// Check write permission before any potentially allocating type coercion.
    pub fn check_store(&self, idx: u32) -> Result<GlobalInfo, GlobalError> {
        let slot = self
            .slots
            .get(idx as usize)
            .ok_or(GlobalError::InvalidIndex(idx))?;
        if slot.initialized && !slot.info.is_mutable {
            return Err(GlobalError::Immutable(idx));
        }
        Ok(slot.info)
    }

    /// Iterate over all values (for GC root scanning).
    pub fn values(&self) -> &[TaggedValue] {
        &self.values
    }

    /// Update registered value slots during exclusive stopped-world root tracing.
    /// Forwarding preserves each slot's type and initialization state.
    pub fn values_mut(&mut self) -> &mut [TaggedValue] {
        &mut self.values
    }
}

impl Default for GlobalTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod global_tests {
    use super::*;

    #[test]
    fn globals_track_initialization_and_const_permissions_separately_from_values() {
        let mut table = GlobalTable::new();
        table
            .initialize(&[
                GlobalInfo {
                    type_index: type_pool::Intrinsic::Unit.type_index(),
                    is_mutable: false,
                },
                GlobalInfo {
                    type_index: type_pool::Intrinsic::Any.type_index(),
                    is_mutable: true,
                },
            ])
            .unwrap();
        assert_eq!(table.get(0), Err(GlobalError::Uninitialized(0)));
        assert_eq!(
            table.get(u32::MAX),
            Err(GlobalError::InvalidIndex(u32::MAX))
        );
        assert_eq!(
            table.set(2, TaggedValue::UNIT),
            Err(GlobalError::InvalidIndex(2))
        );
        table.set(0, TaggedValue::UNIT).unwrap();
        assert_eq!(table.get(0), Ok(TaggedValue::UNIT));
        assert_eq!(
            table.set(0, TaggedValue::TRUE),
            Err(GlobalError::Immutable(0))
        );
        assert_eq!(table.get(0), Ok(TaggedValue::UNIT));
        table.set(1, TaggedValue::FALSE).unwrap();
        table.set(1, TaggedValue::TRUE).unwrap();
        assert_eq!(table.get(1), Ok(TaggedValue::TRUE));
        assert_eq!(table.values(), &[TaggedValue::UNIT, TaggedValue::TRUE]);
        assert_eq!(
            table.initialize(&[]),
            Err(GlobalError::SchemaAlreadyInitialized)
        );
    }

    #[test]
    fn host_allocations_remain_initialized_mutable_globals() {
        let mut table = GlobalTable::new();
        let index = table.alloc(TaggedValue::NULL);
        assert_eq!(table.get(index), Ok(TaggedValue::NULL));
        table.set(index, TaggedValue::TRUE).unwrap();
        assert_eq!(table.get(index), Ok(TaggedValue::TRUE));
        assert_eq!(
            table.initialize(&[]),
            Err(GlobalError::SchemaAlreadyInitialized)
        );
    }

    #[test]
    fn an_empty_schema_is_still_installed_only_once() {
        let mut table = GlobalTable::new();
        table.initialize(&[]).unwrap();
        assert_eq!(
            table.initialize(&[]),
            Err(GlobalError::SchemaAlreadyInitialized)
        );
    }
}

// ---------------------------------------------------------------------------
// ConstantPool — runtime constant pool
// ---------------------------------------------------------------------------

/// Runtime constant pool (values loaded by LOAD_CONST).
pub struct ConstantPool {
    values: Vec<TaggedValue>,
}

impl ConstantPool {
    pub fn new() -> Self {
        Self { values: Vec::new() }
    }

    pub fn push(&mut self, val: TaggedValue) -> u32 {
        let idx = self.values.len() as u32;
        self.values.push(val);
        idx
    }

    pub fn get(&self, idx: u32) -> TaggedValue {
        self.values[idx as usize]
    }

    /// Iterate over all values (for GC root scanning).
    pub fn values(&self) -> &[TaggedValue] {
        &self.values
    }

    /// Update registered value slots during exclusive stopped-world root tracing.
    /// Forwarding preserves each slot's type and initialization state.
    pub fn values_mut(&mut self) -> &mut [TaggedValue] {
        &mut self.values
    }
}

impl Default for ConstantPool {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ClosureEnv — captured values for a closure
// ---------------------------------------------------------------------------

/// A closure environment: function id + captured values.
///
/// On the heap, a closure is laid out as:
///   word 0: func_id (as raw u64)
///   word 1: capture_count (as raw u64)
///   word 2..: captured TaggedValues
///
/// The ObjectHeader's TypeIndex uses the intrinsic Closure type.
pub struct ClosureEnv;

impl ClosureEnv {
    /// Total payload words for a closure with `capture_count` captures.
    pub fn payload_words(capture_count: u16) -> u16 {
        2 + capture_count // func_id + count + captures
    }

    /// Write a closure payload into allocated memory.
    ///
    /// # Safety
    /// `payload` must point to a valid allocation of at least `payload_words(captures.len())` words.
    pub unsafe fn write(payload: *mut u8, func_id: FuncId, captures: &[TaggedValue]) {
        let words = payload as *mut u64;
        unsafe {
            words.write(func_id.0 as u64);
            words.add(1).write(captures.len() as u64);
            for (i, val) in captures.iter().enumerate() {
                words.add(2 + i).write(val.raw());
            }
        }
    }

    /// Read the function id from a closure payload pointer.
    ///
    /// # Safety
    /// `payload` must point to a valid closure payload.
    pub unsafe fn func_id(payload: *const u8) -> FuncId {
        let words = payload as *const u64;
        FuncId(unsafe { words.read() } as u32)
    }

    /// Read the capture count from a closure payload pointer.
    ///
    /// # Safety
    /// `payload` must point to a valid closure payload.
    pub unsafe fn capture_count(payload: *const u8) -> u32 {
        let words = payload as *const u64;
        (unsafe { words.add(1).read() }) as u32
    }

    /// Read a captured value by index from a closure payload pointer.
    ///
    /// # Safety
    /// `payload` must point to a valid closure payload, and `idx < capture_count`.
    pub unsafe fn get_capture(payload: *const u8, idx: u32) -> TaggedValue {
        let words = payload as *const u64;
        TaggedValue::from_raw(unsafe { words.add(2 + idx as usize).read() })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_values_keep_pool_local_indices_and_distinct_immediate_identity() {
        for raw in [0, 42, u32::MAX - 1] {
            let ty = TypeIndex::from_raw(raw);
            let value = TaggedValue::from_type(ty).unwrap();
            assert_eq!(value.as_type(), Some(ty));
            assert!(value.is_immediate());
            assert!(!value.is_heap());
            assert!(!value.is_null());
            assert!(!value.is_unit());
            assert_eq!(value.as_i64(), None);
            assert_eq!(value.as_u64(), None);
            assert_eq!(value.as_f64(), None);
            assert_eq!(value.as_bool(), None);
            assert_eq!(value.as_char(), None);
        }
        assert_ne!(
            TaggedValue::from_type(TypeIndex::from_raw(0)),
            TaggedValue::from_type(TypeIndex::from_raw(1))
        );
        assert_eq!(TaggedValue::TRUE.as_type(), None);
        assert_eq!(TaggedValue::from_i64(42).as_type(), None);
    }

    #[test]
    fn enum_immediates_preserve_identity_and_reject_legacy_symbols() {
        for ty in [0, 1, u32::MAX - 1] {
            for variant in [0, (1 << 25) - 1] {
                let ty = TypeIndex::from_raw(ty);
                let value = TaggedValue::from_enum(ty, variant).unwrap();
                assert_eq!(value.as_enum(), Some((ty, variant)));
                assert_eq!(value.as_type(), None);
                assert_eq!(TaggedValue::from_symbol(value.imm_data()).as_enum(), None);
            }
        }
        assert_eq!(TaggedValue::from_enum(TypeIndex::INVALID, 0), None);
        assert_eq!(
            TaggedValue::from_enum(TypeIndex::from_raw(0), 1 << 25),
            None
        );
        assert_eq!(
            TaggedValue::from_raw(TaggedValue::make_imm(IMM_ENUM, u64::from(u32::MAX) << 25).raw())
                .as_enum(),
            None
        );
        assert_ne!(
            TaggedValue::from_enum(TypeIndex::from_raw(1), 0),
            TaggedValue::from_enum(TypeIndex::from_raw(2), 0)
        );
    }

    #[test]
    fn type_values_reject_invalid_indices_and_oversized_raw_payloads() {
        assert_eq!(TaggedValue::from_type(TypeIndex::INVALID), None);
        for raw in [u64::from(u32::MAX), 1_u64 << 32, (1_u64 << 57) - 1] {
            let malformed = TaggedValue::from_raw(
                TAG_IMM | (IMM_TYPE << IMM_SUB_SHIFT) | (raw << IMM_DATA_SHIFT),
            );
            assert_eq!(malformed.as_type(), None);
        }
    }

    #[test]
    fn tagged_value_i64() {
        let v = TaggedValue::from_i64(42);
        assert_eq!(v.as_i64(), Some(42));
        let v2 = TaggedValue::from_i64(-100);
        assert_eq!(v2.as_i64(), Some(-100));
    }

    #[test]
    fn tagged_value_u64() {
        let v = TaggedValue::from_u64(999);
        assert_eq!(v.as_u64(), Some(999));
    }

    #[test]
    fn tagged_value_bool() {
        assert_eq!(TaggedValue::TRUE.as_bool(), Some(true));
        assert_eq!(TaggedValue::FALSE.as_bool(), Some(false));
    }

    #[test]
    fn tagged_value_null_unit() {
        assert!(TaggedValue::NULL.is_null());
        assert!(TaggedValue::UNIT.is_unit());
        assert!(!TaggedValue::NULL.is_unit());
        assert!(!TaggedValue::UNIT.is_null());
    }

    #[test]
    fn tagged_value_char() {
        let v = TaggedValue::from_char('A');
        assert_eq!(v.as_char(), Some('A'));
    }

    #[test]
    fn register_file_ops() {
        let mut rf = RegisterFile::new();
        rf.set(Reg(0), TaggedValue::from_i64(10));
        rf.set(Reg(1), TaggedValue::from_i64(20));
        assert_eq!(rf.get(Reg(0)).as_i64(), Some(10));
        assert_eq!(rf.get(Reg(1)).as_i64(), Some(20));
    }
}
