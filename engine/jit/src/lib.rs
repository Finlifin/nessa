//! Tiered Hot-Loop & Function JIT compiler with On-Stack Replacement (OSR) for Nessa.
//!
//! Architecture overview:
//! - **CodeCache (`code_cache`)**: Strict POSIX W^X (`PROT_READ | PROT_WRITE` ->
//!   `PROT_READ | PROT_EXEC`) executable memory allocator with a configurable
//!   per-VM byte cap (16 MiB default).
//! - **Tier-1 (`tier1`)**: Fast whole-function native compiler eliminating bytecode
//!   fetch/decode/dispatch overhead, inlining 57-bit `IMM_I_SMALL` arithmetic/comparisons
//!   with hardware `JO` overflow guards, monomorphic `LoadField`/`StoreField` inline
//!   caches, inline safepoint checks, and recording per-PC native offsets for instant
//!   loop back-edge OSR and post-call re-entry.
//! - **Tier-2 (`tier2`)**: Natural-loop specializing compiler performing live-in and
//!   definite-assignment analysis over local slots and virtual registers, unboxing
//!   loop-carried `i64` values into 64-bit GP registers and hybrid `f64` values
//!   (both compressed `IMM_F64` and 1-word heap-boxed `F64`) into SSE2 `XMM` registers
//!   with zero heap allocations inside hot loops and exact deoptimization on overflow.

pub mod arch;
pub mod code_cache;
#[cfg(all(target_arch = "x86_64", unix))]
pub mod tier1;
#[cfg(all(target_arch = "x86_64", unix))]
pub mod tier2;
#[cfg(all(target_arch = "x86_64", unix))]
pub mod x86_64;

use std::ffi::c_void;
use std::pin::Pin;

use gc::ObjectHeader;
use nsbc::FuncId;
use runtime::TaggedValue;
use type_pool::{Intrinsic, TypePool};

pub use code_cache::{CodeCache, DEFAULT_MAX_CODE_BYTES, ExecutableBuffer, JitError};

// ---------------------------------------------------------------------------
// Compile-time runtime layout & bit-tag verification
// ---------------------------------------------------------------------------

/// `TaggedValue` and `ObjectHeader` bit-level constants verified against `runtime` and `gc`.
pub mod layout {
    use super::*;

    pub const TAG_MASK: u64 = 0b111;
    pub const ROT_F64_TAG_MASK: u64 = 0b110;
    pub const IMM_LOW7_MASK: u64 = 0x7f;
    pub const IMM_I_SMALL_TAG: u64 = 0x01;
    pub const IMM_U_SMALL_TAG: u64 = 0x09;
    pub const IMM_F64_TAG: u64 = 0x11;
    pub const IMM_USIZE_TAG: u64 = 0x59;
    pub const IMM_ISIZE_TAG: u64 = 0x61;
    pub const IMM_BOOL_MASK_NO_BIT7: i32 = !0x80;
    pub const TAG_FALSE: u64 = 0x19;
    pub const TAG_TRUE: u64 = 0x99;
    pub const TAG_NULL: u64 = 0x29;
    pub const TAG_UNIT: u64 = 0x31;

    /// Negative offset from heap payload pointer to `ObjectHeader::type_index` (`u32`).
    pub const HDR_TYPE_INDEX_DISP: i32 = -16;
    /// Negative offset from heap payload pointer to `ObjectHeader::gc_meta` (`u32`).
    pub const HDR_GC_META_DISP: i32 = -12;
    /// Mask extracting `PayloadRole` (bits `19..=21`) + `payload_words` (bits `3..=18`),
    /// ignoring MMTk mark/forwarding bits `0..=2`.
    pub const GC_META_ROLE_AND_WORDS_MASK: i32 = 0x003F_FFF8;
    /// Expected `(gc_meta & GC_META_ROLE_AND_WORDS_MASK)` for a 1-word `Ordinary` box (`1 << 3`).
    pub const GC_META_ORDINARY_1WORD: i32 = 1 << 3;
    /// Expected `(gc_meta & GC_META_ROLE_AND_WORDS_MASK)` for a 3-word `Ordinary` wrapper (`3 << 3`).
    pub const GC_META_ORDINARY_3WORDS: i32 = 3 << 3;
    /// Mask extracting only `PayloadRole` bits `19..=21` (`0` == `Ordinary`).
    pub const GC_META_ROLE_MASK: i32 = 0x0038_0000;

    pub const INTRINSIC_I64_RAW: u32 = Intrinsic::I64.type_index().as_u32();
    pub const INTRINSIC_U64_RAW: u32 = Intrinsic::U64.type_index().as_u32();
    pub const INTRINSIC_USIZE_RAW: u32 = Intrinsic::Usize.type_index().as_u32();
    pub const INTRINSIC_ISIZE_RAW: u32 = Intrinsic::Isize.type_index().as_u32();
    pub const INTRINSIC_F64_RAW: u32 = Intrinsic::F64.type_index().as_u32();
    pub const INTRINSIC_BOOL_RAW: u32 = Intrinsic::Bool.type_index().as_u32();

    const _: () = {
        assert!(ObjectHeader::SIZE == 16);
        assert!(std::mem::offset_of!(ObjectHeader, type_index) == 0);
        assert!(std::mem::offset_of!(ObjectHeader, gc_meta) == 4);
        assert!(INTRINSIC_I64_RAW == 9);
        assert!(INTRINSIC_F64_RAW == 13);
        assert!(INTRINSIC_BOOL_RAW == 14);
    };

    #[inline]
    pub fn debug_assert_runtime_tags() {
        debug_assert_eq!(TaggedValue::FALSE.raw(), TAG_FALSE);
        debug_assert_eq!(TaggedValue::TRUE.raw(), TAG_TRUE);
        debug_assert_eq!(TaggedValue::NULL.raw(), TAG_NULL);
        debug_assert_eq!(TaggedValue::UNIT.raw(), TAG_UNIT);
        debug_assert_eq!(TaggedValue::from_i64(0).raw(), IMM_I_SMALL_TAG);
        debug_assert_eq!(TaggedValue::from_i64(1).raw(), (1 << 7) | IMM_I_SMALL_TAG);
        debug_assert_eq!(TaggedValue::from_f64(0.0).raw(), IMM_F64_TAG);
        debug_assert_eq!(
            TaggedValue::from_f64(1.5).raw(),
            1.5_f64.to_bits().rotate_left(4)
        );
        debug_assert_eq!(TaggedValue::try_from_usize(0).unwrap().raw(), IMM_USIZE_TAG);
        debug_assert_eq!(TaggedValue::try_from_isize(0).unwrap().raw(), IMM_ISIZE_TAG);
    }
}

// ---------------------------------------------------------------------------
// Profiling thresholds & JIT modes
// ---------------------------------------------------------------------------

/// Hotness added per function entry (`pc == 0`).
pub const ENTRY_HOTNESS_INCREMENT: u64 = 16;
/// Hotness added per taken backward jump (`offset <= 0`).
pub const BACKEDGE_HOTNESS_INCREMENT: u64 = 1;
/// Hotness threshold for Tier-1 compilation + immediate loop OSR in `Auto` mode.
pub const TIER1_HOTNESS_THRESHOLD: u64 = 64;
/// Hotness threshold for Tier-2 loop specialization in `Auto` mode.
pub const TIER2_HOTNESS_THRESHOLD: u64 = 512;
/// Maximum cumulative Tier-2 deoptimizations per function before disabling Tier-2 for that function.
pub const MAX_DEOPTS_PER_FUNCTION: u32 = 8;

/// Execution / compilation mode for the JIT subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JitMode {
    /// Default tiered mode: profile in interpreter, Tier-1 + OSR at `4,096`, Tier-2 at `65,536`.
    #[default]
    Auto,
    /// Disable JIT completely; run all functions in the bytecode interpreter.
    Off,
    /// Compile functions eagerly at Tier-1 without Tier-2 promotion.
    Tier1,
    /// Compile functions eagerly at Tier-2 (falling back to Tier-1 if no natural numeric loop exists).
    Tier2,
}

impl JitMode {
    /// Parse a CLI or environment mode string (`auto`, `off`, `tier1`, `tier2`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "off" | "false" | "0" | "none" => Some(Self::Off),
            "tier1" | "1" => Some(Self::Tier1),
            "tier2" | "2" => Some(Self::Tier2),
            _ => None,
        }
    }

    /// Resolve mode from `NESSA_JIT` environment variable, defaulting to [`JitMode::Auto`].
    pub fn from_env() -> Self {
        match std::env::var("NESSA_JIT") {
            Ok(val) if !val.trim().is_empty() => Self::parse(&val).unwrap_or(Self::Auto),
            _ => Self::Auto,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Tier1 => "tier1",
            Self::Tier2 => "tier2",
        }
    }
}

/// Per-VM JIT configuration.
#[derive(Debug, Clone, Copy)]
pub struct JitConfig {
    pub mode: JitMode,
    pub max_code_bytes: usize,
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            mode: JitMode::from_env(),
            max_code_bytes: DEFAULT_MAX_CODE_BYTES,
        }
    }
}

impl JitConfig {
    pub fn with_mode(mode: JitMode) -> Self {
        Self {
            mode,
            max_code_bytes: DEFAULT_MAX_CODE_BYTES,
        }
    }
}

/// Read-only runtime JIT telemetry exposed via `std.runtime`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitStats {
    pub compiled_functions: usize,
    pub code_bytes: usize,
    pub deopt_count: usize,
}

// ---------------------------------------------------------------------------
// Externalized Trampoline ABI, Exit Codes, Runtime Context & Inline Caches
// ---------------------------------------------------------------------------

/// Reason native JIT execution returned control to `Vm::execute_task`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitExitKind {
    /// Execution reached `pc >= instructions.len()`.
    Finished = 0,
    /// `Return` or `ReturnUnit` completed with `value` holding the raw `TaggedValue`.
    Return = 1,
    /// Resume interpreter dispatch at `pc` (calls, builtins, effects, continuations, or Tier-2 deopt).
    InterpretAt = 2,
    /// Tier-1 loop back-edge crossed `TIER2_HOTNESS_THRESHOLD`; upgrade to Tier-2 and OSR at `pc`.
    TierUp = 3,
    /// Cooperative safepoint yielded the task time-slice; `task.pc` should be `pc`.
    Yield = 4,
    /// Helper requested suspend-retry at `pc`.
    SuspendRetry = 5,
    /// Task was observed cancelled at safepoint.
    Cancelled = 6,
    /// Runtime error (`VmError`) stored in the bridge's `pending_error` slot.
    Trap = 7,
    /// Nested call/helper already positioned `task.current_func` and `task.pc`; resume `execute_task` loop.
    ContinueTaskLoop = 8,
    /// Nested call/helper already recorded `task.pc` and yielded the slice.
    YieldAtTaskPc = 9,
    /// Nested call/helper already recorded `task.pc` and suspended the task.
    SuspendAtTaskPc = 10,
}

/// Structured exit descriptor written via `out_exit: *mut JitExit` on return from native code.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct JitExit {
    pub kind: u32,
    pub pc: u32,
    pub value: u64,
}

impl JitExit {
    pub const OFF_KIND: i32 = std::mem::offset_of!(Self, kind) as i32;
    pub const OFF_PC: i32 = std::mem::offset_of!(Self, pc) as i32;
    pub const OFF_VALUE: i32 = std::mem::offset_of!(Self, value) as i32;
}

/// Value representation check required for non-coercing inline `StoreField` IC hits.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStoreFastKind {
    /// Always call the slow helper (coercions, narrow ints, traits, polymorphic site).
    SlowOnly = 0,
    /// `Intrinsic::Any`: any `TaggedValue` is valid without coercion.
    Any = 1,
    /// `Intrinsic::I64`: inline hit when `(val & 0x7f) == 0x01` (`IMM_I_SMALL`).
    SmallI64 = 2,
    /// `Intrinsic::Bool`: inline hit when `(val & !0x80) == 0x19`.
    Bool = 3,
    /// `Intrinsic::F64`: inline hit when `(val & 6) != 0`, `(val & 0x7f) == 0x11`, or 1-word `F64` box.
    CanonicalF64 = 4,
    /// Nominal `Struct`: inline hit when `val` is heap `Ordinary` with `type_index == expected_val_type`.
    ExactHeapNominal = 5,
    /// `?Struct`: inline hit when `val == TAG_NULL` or heap `Ordinary` with `type_index == expected_val_type`.
    OptionalNominal = 6,
    /// `?i64`: inline hit when `val == TAG_NULL` or `(val & 0x7f) == 0x01`.
    OptionalSmallI64 = 7,
    /// `usize`: inline hit when `(val & 0x7f) == 0x59` (`IMM_USIZE`).
    Usize = 8,
}

/// Per-site monomorphic inline cache for `LoadField` and `StoreField`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MonomorphicIc {
    /// Cached `ObjectHeader::type_index` (`u32::MAX` when cold, `u32::MAX - 1` when megamorphic).
    pub receiver_type: u32,
    /// Cached `(payload_words as u32) << 3` matched against `header.gc_meta & 0x003F_FFF8`.
    pub expected_gc_meta_masked: u32,
    /// For `LoadField`: `1` if direct non-`UNIT` read is valid; for `StoreField`: [`FieldStoreFastKind`].
    pub fast_kind: u32,
    /// Expected `type_index` for [`FieldStoreFastKind::ExactHeapNominal`].
    pub expected_val_type: u32,
}

impl Default for MonomorphicIc {
    fn default() -> Self {
        Self::cold()
    }
}

impl MonomorphicIc {
    pub const COLD_TYPE: u32 = u32::MAX;
    pub const MEGAMORPHIC_TYPE: u32 = u32::MAX - 1;

    pub const OFF_RECEIVER_TYPE: i32 = std::mem::offset_of!(Self, receiver_type) as i32;
    pub const OFF_EXPECTED_GC_META: i32 =
        std::mem::offset_of!(Self, expected_gc_meta_masked) as i32;
    pub const OFF_FAST_KIND: i32 = std::mem::offset_of!(Self, fast_kind) as i32;
    pub const OFF_EXPECTED_VAL_TYPE: i32 = std::mem::offset_of!(Self, expected_val_type) as i32;

    pub const fn cold() -> Self {
        Self {
            receiver_type: Self::COLD_TYPE,
            expected_gc_meta_masked: 0,
            fast_kind: 0,
            expected_val_type: u32::MAX,
        }
    }
}

/// Address-stable per-invocation bridge state passed as `vm_opaque` to native JIT code.
///
/// Whenever a runtime helper runs (which may allocate or trigger an MMTk Immix moving GC
/// or resize `local_slots` / `ConstantPool`), the helper refreshes `regs_ptr`, `slots_ptr`,
/// `slots_len`, `consts_ptr`, `consts_len`, and `has_other_ready` before returning to JIT code.
#[repr(C)]
pub struct JitRuntimeCtx {
    /// Opaque pointer owned by `interpreter::jit_bridge` (`*mut BridgeSession`).
    pub host_session: *mut c_void,
    /// Current `ctx.registers.regs.as_mut_ptr().cast::<u64>()`.
    pub regs_ptr: *mut u64,
    /// Current `ctx.local_slots.as_mut_ptr().cast::<u64>()`.
    pub slots_ptr: *mut u64,
    /// Current `ctx.local_slots.len()`.
    pub slots_len: usize,
    /// Current `roots.constants.values().as_ptr().cast::<u64>()`.
    pub consts_ptr: *const u64,
    /// Current `roots.constants.values().len()`.
    pub consts_len: usize,
    /// Pointer to `GC_FLAGS.gc_requested` byte.
    pub gc_requested_ptr: *const u8,
    /// Pointer to `GC_FLAGS.preempt_requested` byte.
    pub preempt_requested_ptr: *const u8,
    /// Pointer to `SharedVm::aux_started` byte (`0` in single-worker mode).
    pub aux_started_ptr: *const u8,
    /// Remaining safepoint budget in the current task slice.
    pub slice_safepoints: u32,
    /// `1` if scheduler currently has other ready tasks, `0` otherwise.
    pub has_other_ready: u8,
    /// `1` if Tier-1 backward jumps should exit with `TierUp` once `hotness >= TIER2_HOTNESS_THRESHOLD`.
    pub allow_tier_up: u8,
    /// `1` if Tier-2 unboxed loops are enabled for this invocation (`0` after deopt cap).
    pub allow_tier2_loops: u8,
    pub _pad: u8,
}

impl JitRuntimeCtx {
    pub const OFF_REGS_PTR: i32 = std::mem::offset_of!(Self, regs_ptr) as i32;
    pub const OFF_SLOTS_PTR: i32 = std::mem::offset_of!(Self, slots_ptr) as i32;
    pub const OFF_SLOTS_LEN: i32 = std::mem::offset_of!(Self, slots_len) as i32;
    pub const OFF_CONSTS_PTR: i32 = std::mem::offset_of!(Self, consts_ptr) as i32;
    pub const OFF_GC_REQ_PTR: i32 = std::mem::offset_of!(Self, gc_requested_ptr) as i32;
    pub const OFF_PREEMPT_REQ_PTR: i32 = std::mem::offset_of!(Self, preempt_requested_ptr) as i32;
    pub const OFF_AUX_STARTED_PTR: i32 = std::mem::offset_of!(Self, aux_started_ptr) as i32;
    pub const OFF_SLICE_SAFEPOINTS: i32 = std::mem::offset_of!(Self, slice_safepoints) as i32;
    pub const OFF_HAS_OTHER_READY: i32 = std::mem::offset_of!(Self, has_other_ready) as i32;
    pub const OFF_ALLOW_TIER_UP: i32 = std::mem::offset_of!(Self, allow_tier_up) as i32;
    pub const OFF_ALLOW_TIER2_LOOPS: i32 = std::mem::offset_of!(Self, allow_tier2_loops) as i32;
}

/// Trampoline function signature on `x86_64` Unix (`SysV64` calling convention).
#[cfg(all(target_arch = "x86_64", unix))]
pub type JitEntryFn = unsafe extern "sysv64" fn(
    vm_opaque: *mut c_void,
    regs_ptr: *mut u64,
    entry_native_offset: usize,
    out_exit: *mut JitExit,
);

/// Trampoline function signature on non-`x86_64` targets.
#[cfg(not(all(target_arch = "x86_64", unix)))]
pub type JitEntryFn = unsafe extern "C" fn(
    vm_opaque: *mut c_void,
    regs_ptr: *mut u64,
    entry_native_offset: usize,
    out_exit: *mut JitExit,
);

/// Runtime helper callbacks invoked from compiled JIT slow paths.
///
/// Every callback returns `0` (`Continue`) or a non-zero [`JitExitKind`] discriminant
/// (`Yield`, `SuspendRetry`, `Cancelled`, or `Trap`). All managed values reside in
/// `StackContext` prior to any callback invocation.
#[cfg(all(target_arch = "x86_64", unix))]
#[derive(Clone, Copy)]
pub struct JitHelpers {
    pub alloc_slots: unsafe extern "sysv64" fn(vm_opaque: *mut c_void, count: u32) -> u32,
    pub safepoint: unsafe extern "sysv64" fn(vm_opaque: *mut c_void, pc: u32) -> u32,
    pub numeric_slow: unsafe extern "sysv64" fn(vm_opaque: *mut c_void, instr_word: u32) -> u32,
    pub cmp_slow: unsafe extern "sysv64" fn(vm_opaque: *mut c_void, instr_word: u32) -> u32,
    pub is_truthy: unsafe extern "sysv64" fn(raw_val: u64) -> u32,
    pub load_const_slow:
        unsafe extern "sysv64" fn(vm_opaque: *mut c_void, dst_reg: u32, const_idx: u32) -> u32,
    pub type_op: unsafe extern "sysv64" fn(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32,
    pub load_field_ic: unsafe extern "sysv64" fn(
        vm_opaque: *mut c_void,
        pc: u32,
        instr_word: u32,
        ic_ptr: *mut MonomorphicIc,
    ) -> u32,
    pub store_field_ic: unsafe extern "sysv64" fn(
        vm_opaque: *mut c_void,
        pc: u32,
        instr_word: u32,
        ic_ptr: *mut MonomorphicIc,
    ) -> u32,
    pub store_boxed_f64: unsafe extern "sysv64" fn(
        vm_opaque: *mut c_void,
        is_slot: u32,
        index: u32,
        f64_bits: u64,
    ) -> u32,
    pub step_inline_op:
        unsafe extern "sysv64" fn(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32,
    pub alloc_object_fast: unsafe extern "sysv64" fn(
        vm_opaque: *mut c_void,
        dst_reg: u32,
        canon_ty_raw: u32,
        words: u32,
    ) -> u32,
    pub call_builtin: unsafe extern "sysv64" fn(
        vm_opaque: *mut c_void,
        pc: u32,
        builtin_id: u32,
        arg_count: u32,
    ) -> u32,
    pub call_op: unsafe extern "sysv64" fn(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32,
    pub call_direct_fast: unsafe extern "sysv64" fn(
        vm_opaque: *mut c_void,
        return_pc: u32,
        callee_raw: u32,
        packed_slots_and_cs: u32,
    ) -> u32,
}

#[cfg(not(all(target_arch = "x86_64", unix)))]
#[derive(Clone, Copy)]
pub struct JitHelpers {
    pub alloc_slots: unsafe extern "C" fn(vm_opaque: *mut c_void, count: u32) -> u32,
    pub safepoint: unsafe extern "C" fn(vm_opaque: *mut c_void, pc: u32) -> u32,
    pub numeric_slow: unsafe extern "C" fn(vm_opaque: *mut c_void, instr_word: u32) -> u32,
    pub cmp_slow: unsafe extern "C" fn(vm_opaque: *mut c_void, instr_word: u32) -> u32,
    pub is_truthy: unsafe extern "C" fn(raw_val: u64) -> u32,
    pub load_const_slow:
        unsafe extern "C" fn(vm_opaque: *mut c_void, dst_reg: u32, const_idx: u32) -> u32,
    pub type_op: unsafe extern "C" fn(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32,
    pub load_field_ic: unsafe extern "C" fn(
        vm_opaque: *mut c_void,
        pc: u32,
        instr_word: u32,
        ic_ptr: *mut MonomorphicIc,
    ) -> u32,
    pub store_field_ic: unsafe extern "C" fn(
        vm_opaque: *mut c_void,
        pc: u32,
        instr_word: u32,
        ic_ptr: *mut MonomorphicIc,
    ) -> u32,
    pub store_boxed_f64: unsafe extern "C" fn(
        vm_opaque: *mut c_void,
        is_slot: u32,
        index: u32,
        f64_bits: u64,
    ) -> u32,
    pub step_inline_op:
        unsafe extern "C" fn(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32,
    pub alloc_object_fast: unsafe extern "C" fn(
        vm_opaque: *mut c_void,
        dst_reg: u32,
        canon_ty_raw: u32,
        words: u32,
    ) -> u32,
    pub call_builtin: unsafe extern "C" fn(
        vm_opaque: *mut c_void,
        pc: u32,
        builtin_id: u32,
        arg_count: u32,
    ) -> u32,
    pub call_op: unsafe extern "C" fn(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32,
    pub call_direct_fast: unsafe extern "C" fn(
        vm_opaque: *mut c_void,
        return_pc: u32,
        callee_raw: u32,
        packed_slots_and_cs: u32,
    ) -> u32,
}

// ---------------------------------------------------------------------------
// Compile-time Constant Classification & Compilation Input
// ---------------------------------------------------------------------------

/// Pre-inspected constant-pool entry supplied to the JIT compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileConstant {
    /// Validated immediate `TaggedValue` whose raw bits never change across GC.
    ImmediateValidated {
        raw: u64,
        /// `Some(val)` when this constant is a 57-bit signed `IMM_I_SMALL`.
        i64_val: Option<i64>,
        /// `Some(bits)` when this constant is a compressed `IMM_F64`.
        f64_bits: Option<u64>,
    },
    /// Validated heap-allocated constant residing in `ConstantPool` (must be loaded via
    /// `consts_ptr` in Tier-1 because Immix may relocate it; if `f64_bits` is `Some`,
    /// Tier-2 can load its immutable IEEE-754 scalar bits directly into SSE2!).
    HeapObject { f64_bits: Option<u64> },
    /// Constant requiring runtime validation via `load_const_slow`.
    Slow,
}

/// Read-only view of function bytecode and VM metadata needed during compilation.
pub struct CompilationInput<'a> {
    pub func_id: FuncId,
    pub instructions: &'a [u32],
    pub constants: &'a [CompileConstant],
    pub type_pool: &'a TypePool,
    pub has_scope: &'a dyn Fn(usize) -> bool,
    pub helpers: &'a JitHelpers,
    pub hotness_ptr: *mut u64,
    pub global_deopt_ptr: *mut u64,
    pub func_deopt_ptr: *mut u32,
    /// Number of local slots statically guaranteed allocated for all `pc > 0`.
    pub guaranteed_slots: u32,
    /// Canonical `TypeIndex` of `std.collection.List`, when registered.
    pub list_type_raw: Option<u32>,
    /// Canonical `TypeIndex` of `List`'s backing `Buffer`, when registered.
    pub list_buf_type_raw: Option<u32>,
    /// `true` when `LIST_LEN` / `LIST_GET` / `LIST_SET` have not been overridden by host hooks.
    pub fast_list_builtins: bool,
    /// Returns `Some((guaranteed_slots, clobbers_callee_saved))` when `callee` is a plain
    /// non-closure, non-display, value-only function with `param_count == arg_count`.
    pub fast_direct_callee_slots: &'a dyn Fn(FuncId, u8) -> Option<(u32, bool)>,
}

/// Output produced by a backend compiler before sealing in [`CodeCache`].
pub struct EmittedCode {
    pub bytes: Vec<u8>,
    /// Native byte offset for each bytecode PC (`u32::MAX` if not directly enterable).
    pub pc_offsets: Box<[u32]>,
    /// Pinned monomorphic inline cache array referenced by raw pointer from `bytes`.
    pub ics: Pin<Box<[MonomorphicIc]>>,
    /// Number of specialized Tier-2 loops emitted (`0` for Tier-1).
    pub specialized_loops: usize,
}

/// Installed native artifact for a single tier of a function.
pub struct CompiledFunctionArtifact {
    code_ptr: *const u8,
    pc_offsets: Box<[u32]>,
    _ics: Pin<Box<[MonomorphicIc]>>,
}

impl CompiledFunctionArtifact {
    /// Entry pointer and native byte offset to jump to `pc` (`0` for `pc == 0`).
    #[inline]
    pub fn entry_for_pc(&self, pc: u32) -> Option<(JitEntryFn, usize)> {
        let offset = if pc == 0 {
            0
        } else {
            let off = *self.pc_offsets.get(pc as usize)?;
            if off == u32::MAX {
                return None;
            }
            off as usize
        };
        // SAFETY: `self.code_ptr` points to the start of a sealed `PROT_READ | PROT_EXEC`
        // `ExecutableBuffer` owned by the enclosing `JitEngine::cache` with matching ABI.
        let entry: JitEntryFn = unsafe { std::mem::transmute(self.code_ptr) };
        Some((entry, offset))
    }
}

// ---------------------------------------------------------------------------
// Per-VM JitEngine
// ---------------------------------------------------------------------------

struct JitCell {
    hotness: Box<u64>,
    func_deopts: Box<u32>,
    tier1: Option<CompiledFunctionArtifact>,
    tier2: Option<CompiledFunctionArtifact>,
    tier1_failed: bool,
    tier2_ineligible: bool,
    counted_compiled: bool,
}

impl JitCell {
    fn new() -> Self {
        Self {
            hotness: Box::new(0),
            func_deopts: Box::new(0),
            tier1: None,
            tier2: None,
            tier1_failed: false,
            tier2_ineligible: false,
            counted_compiled: false,
        }
    }
}

/// Top-level per-VM JIT manager owning profiling counters, inline caches, and W^X code cache.
pub struct JitEngine {
    config: JitConfig,
    cache: CodeCache,
    cells: Vec<JitCell>,
    compiled_functions: usize,
    total_deopt_count: Box<u64>,
}

impl JitEngine {
    pub fn new(config: JitConfig) -> Self {
        layout::debug_assert_runtime_tags();
        Self {
            cache: CodeCache::new(config.max_code_bytes),
            config,
            cells: Vec::new(),
            compiled_functions: 0,
            total_deopt_count: Box::new(0),
        }
    }

    #[inline]
    pub fn config(&self) -> JitConfig {
        self.config
    }

    pub fn set_config(&mut self, config: JitConfig) {
        self.invalidate_all();
        self.config = config;
        self.cache = CodeCache::new(config.max_code_bytes);
    }

    /// Whether JIT execution is enabled on the current target and configuration.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.config.mode != JitMode::Off && arch::is_target_supported()
    }

    #[inline]
    pub fn stats(&self) -> JitStats {
        JitStats {
            compiled_functions: self.compiled_functions,
            code_bytes: self.cache.code_bytes(),
            deopt_count: (*self.total_deopt_count) as usize,
        }
    }

    /// Clear all compiled code and inline caches when VM type metadata or scopes change.
    pub fn invalidate_all(&mut self) {
        for cell in &mut self.cells {
            cell.tier1 = None;
            cell.tier2 = None;
            cell.tier1_failed = false;
            cell.tier2_ineligible = false;
            cell.counted_compiled = false;
            *cell.hotness = 0;
            *cell.func_deopts = 0;
        }
        self.cache.clear();
        self.compiled_functions = 0;
        *self.total_deopt_count = 0;
    }

    fn ensure_cell(&mut self, func_id: FuncId) -> &mut JitCell {
        let idx = func_id.0 as usize;
        if idx >= self.cells.len() {
            self.cells.resize_with(idx + 1, JitCell::new);
        }
        &mut self.cells[idx]
    }

    /// Look up an already-compiled Tier-2 or Tier-1 entry point for `(func_id, pc)`.
    ///
    /// Returns `(entry_fn, native_offset, allow_tier_up, allow_tier2_loops)`.
    #[inline]
    pub fn lookup_entry(
        &self,
        func_id: FuncId,
        pc: u32,
    ) -> Option<(JitEntryFn, usize, bool, bool)> {
        if !self.is_enabled() {
            return None;
        }
        let cell = self.cells.get(func_id.0 as usize)?;
        let deopt_capped = *cell.func_deopts >= MAX_DEOPTS_PER_FUNCTION;
        if !deopt_capped
            && let Some(t2) = &cell.tier2
            && let Some((entry, offset)) = t2.entry_for_pc(pc)
        {
            return Some((entry, offset, false, true));
        }
        if let Some(t1) = &cell.tier1
            && let Some((entry, offset)) = t1.entry_for_pc(pc)
        {
            let allow_tier_up =
                self.config.mode == JitMode::Auto && !cell.tier2_ineligible && !deopt_capped;
            return Some((entry, offset, allow_tier_up, false));
        }
        None
    }

    /// Record a function entry or backward-jump hotness increment and return `true` if the
    /// function should now be compiled (for entry or immediate loop OSR).
    #[inline]
    pub fn record_hotness(&mut self, func_id: FuncId, delta: u64) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let mode = self.config.mode;
        let cell = self.ensure_cell(func_id);
        if cell.tier1_failed {
            return false;
        }
        if cell.tier2.is_some() || (cell.tier1.is_some() && mode != JitMode::Tier2) {
            return false;
        }
        *cell.hotness = cell.hotness.saturating_add(delta);
        match mode {
            JitMode::Off => false,
            JitMode::Tier1 | JitMode::Tier2 => cell.tier1.is_none() && cell.tier2.is_none(),
            JitMode::Auto => cell.tier1.is_none() && *cell.hotness >= TIER1_HOTNESS_THRESHOLD,
        }
    }

    /// Pointers to per-function hotness/deopt counters for [`CompilationInput`].
    pub fn counter_ptrs(&mut self, func_id: FuncId) -> (*mut u64, *mut u64, *mut u32) {
        let global_deopt_ptr: *mut u64 = &mut *self.total_deopt_count;
        let cell = self.ensure_cell(func_id);
        let hotness_ptr: *mut u64 = &mut *cell.hotness;
        let func_deopt_ptr: *mut u32 = &mut *cell.func_deopts;
        (hotness_ptr, global_deopt_ptr, func_deopt_ptr)
    }

    /// Compile `func_id` according to the current mode (or force `tier2` on `TierUp`).
    /// Failures (including `CodeCacheFull`) degrade gracefully to the interpreter or Tier-1.
    pub fn compile_function(&mut self, input: &CompilationInput<'_>, request_tier2: bool) {
        if !self.is_enabled() {
            return;
        }
        let want_tier2 = request_tier2 || self.config.mode == JitMode::Tier2;
        let idx = input.func_id.0 as usize;
        self.ensure_cell(input.func_id);

        if want_tier2 && !self.cells[idx].tier2_ineligible && self.cells[idx].tier2.is_none() {
            match arch::compile_tier2(input) {
                Ok(emitted) if emitted.specialized_loops > 0 => {
                    match self.cache.allocate(&emitted.bytes) {
                        Ok(buf) => {
                            let code_ptr = buf.as_ptr();
                            let cell = &mut self.cells[idx];
                            cell.tier2 = Some(CompiledFunctionArtifact {
                                code_ptr,
                                pc_offsets: emitted.pc_offsets,
                                _ics: emitted.ics,
                            });
                            if !cell.counted_compiled {
                                cell.counted_compiled = true;
                                self.compiled_functions += 1;
                            }
                            return;
                        }
                        Err(_) => {
                            self.cells[idx].tier2_ineligible = true;
                        }
                    }
                }
                _ => {
                    self.cells[idx].tier2_ineligible = true;
                }
            }
        }

        if self.cells[idx].tier1.is_none() && !self.cells[idx].tier1_failed {
            match arch::compile_tier1(input) {
                Ok(emitted) => match self.cache.allocate(&emitted.bytes) {
                    Ok(buf) => {
                        let code_ptr = buf.as_ptr();
                        let cell = &mut self.cells[idx];
                        cell.tier1 = Some(CompiledFunctionArtifact {
                            code_ptr,
                            pc_offsets: emitted.pc_offsets,
                            _ics: emitted.ics,
                        });
                        if !cell.counted_compiled {
                            cell.counted_compiled = true;
                            self.compiled_functions += 1;
                        }
                    }
                    Err(_) => {
                        self.cells[idx].tier1_failed = true;
                    }
                },
                Err(_) => {
                    self.cells[idx].tier1_failed = true;
                }
            }
        }
    }
}
