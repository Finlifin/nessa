use nsbc::{FuncId, GlobalInfo, Reg};
pub use type_pool::TypeIndex;

pub mod stacks;
pub use stacks::{ContinuationId, PromptId, StackContext, StackError, TaskStacks};
pub mod numeric;
pub use numeric::{Number, NumericError};

pub mod builtin;
pub use builtin::ids;
pub use builtin::{
    BUILTIN_ABI_VERSION, BUILTIN_FN_META, BuiltinFnId, BuiltinFnMeta, lookup_builtin_fn_id,
    lookup_builtin_fn_meta,
};

/// Fast non-cryptographic Fx/SplitMix hasher for internal runtime integer/ID tables.
#[derive(Default)]
pub struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h = (h ^ (b as u64)).wrapping_mul(0x517cc1b727220a95);
        }
        self.0 = h;
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0 = (self.0 ^ (i as u64)).wrapping_mul(0x517cc1b727220a95);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0 ^ (i as u64)).wrapping_mul(0x517cc1b727220a95);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        let x = i ^ (i >> 30);
        self.0 = (self.0 ^ x).wrapping_mul(0xbf58476d1ce4e5b9);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
}

pub type FastHashMap<K, V> =
    std::collections::HashMap<K, V, std::hash::BuildHasherDefault<FastHasher>>;
pub type FastHashSet<T> = std::collections::HashSet<T, std::hash::BuildHasherDefault<FastHasher>>;

// ---------------------------------------------------------------------------
// TaggedValue — the universal 64-bit runtime value
// ---------------------------------------------------------------------------

/// A 64-bit tagged value.  The low 3 bits encode the primary tag:
///
/// ```text
/// [Payload 61 bits][Tag 3 bits]
///   000        HeapObject   — payload is 8-byte-aligned pointer (0 = invalid/uninit)
///   001        Immediate    — payload bits [6:3] hold sub-tag, [63:7] hold 57-bit data
///   010..=111  RotatedF64   — raw == f64::to_bits().rotate_left(4) for any IEEE-754 f64
///                             whose exponent bits [62:60] >= 0b010 ([2^-511, +Inf], +-Inf, NaN)
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct TaggedValue(u64);

// Tag constants
const TAG_MASK: u64 = 0b111;
const TAG_HEAP: u64 = 0b000;
const TAG_IMM: u64 = 0b001;
const ROT_F64_TAG_MASK: u64 = 0b110;

// Immediate sub-tag (bits [6:3] of the value, i.e. bits [3:0] of payload)
const IMM_SUB_SHIFT: u32 = 3;
const IMM_SUB_MASK: u64 = 0xF << IMM_SUB_SHIFT;
const IMM_LOW7_MASK: u64 = TAG_MASK | IMM_SUB_MASK;
const IMM_DATA_SHIFT: u32 = 7; // payload data starts at bit 7

const IMM_I_SMALL: u64 = 0x0;
const IMM_U_SMALL: u64 = 0x1;
const IMM_F64: u64 = 0x2;
const IMM_BOOL: u64 = 0x3;
const IMM_NULL: u64 = 0x5;
const IMM_UNIT: u64 = 0x6;
const IMM_CHAR: u64 = 0x7;
const IMM_TYPE: u64 = 0x8;
const IMM_ENUM: u64 = 0x9;
const IMM_TRAIT_PROOF: u64 = 0xA;
const IMM_USIZE: u64 = 0xB;
const IMM_ISIZE: u64 = 0xC;
const IMM_NARROW: u64 = 0xD;

// Narrow scalar sub-kinds stored in bits [35:32] of IMM_NARROW's 57-bit data payload.
const NARROW_U8: u64 = 0;
const NARROW_U16: u64 = 1;
const NARROW_U32: u64 = 2;
const NARROW_I8: u64 = 3;
const NARROW_I16: u64 = 4;
const NARROW_I32: u64 = 5;
const NARROW_F32: u64 = 6;

impl TaggedValue {
    pub const UNIT: Self = Self::make_imm(IMM_UNIT, 0);
    pub const NULL: Self = Self::make_imm(IMM_NULL, 0);
    pub const TRUE: Self = Self::make_imm(IMM_BOOL, 1);
    pub const FALSE: Self = Self::make_imm(IMM_BOOL, 0);

    const fn make_imm(sub: u64, data: u64) -> Self {
        Self(TAG_IMM | (sub << IMM_SUB_SHIFT) | (data << IMM_DATA_SHIFT))
    }

    #[inline]
    const fn has_imm_sub(self, sub: u64) -> bool {
        (self.0 & IMM_LOW7_MASK) == (TAG_IMM | (sub << IMM_SUB_SHIFT))
    }

    // -- Constructors --------------------------------------------------------

    /// Encode an immediate integer, or return None when heap storage is needed.
    #[inline]
    pub fn try_from_i64(v: i64) -> Option<Self> {
        (-(1_i64 << 56)..(1_i64 << 56))
            .contains(&v)
            .then(|| Self::make_imm(IMM_I_SMALL, (v as u64) & ((1_u64 << 57) - 1)))
    }

    /// Encode an immediate unsigned integer without truncation.
    #[inline]
    pub fn try_from_u64(v: u64) -> Option<Self> {
        (v < (1_u64 << 57)).then(|| Self::make_imm(IMM_U_SMALL, v))
    }

    /// Encode a 57-bit immediate `usize` preserving `Intrinsic::Usize` identity.
    #[inline]
    pub fn try_from_usize(v: u64) -> Option<Self> {
        (v < (1_u64 << 57)).then(|| Self::make_imm(IMM_USIZE, v))
    }

    /// Encode a 57-bit immediate `isize` preserving `Intrinsic::Isize` identity.
    #[inline]
    pub fn try_from_isize(v: i64) -> Option<Self> {
        (-(1_i64 << 56)..(1_i64 << 56))
            .contains(&v)
            .then(|| Self::make_imm(IMM_ISIZE, (v as u64) & ((1_u64 << 57) - 1)))
    }

    /// Encode an inline `f64` using either rotated primary tags `0b010..=0b111`
    /// (covering all floats with magnitude `>= 2^-511`, `+-Inf`, and all `NaN` bit patterns)
    /// or `IMM_F64` (covering `+0.0`, `-0.0`, and dyadic tiny floats with zero low 7 bits).
    #[inline]
    pub fn try_from_f64(v: f64) -> Option<Self> {
        let bits = v.to_bits();
        let rot = bits.rotate_left(4);
        if (rot & ROT_F64_TAG_MASK) != 0 {
            return Some(Self(rot));
        }
        (bits & 0x7f == 0).then(|| Self::make_imm(IMM_F64, bits >> 7))
    }

    /// Encode any concrete numeric scalar inline when representable without heap boxing,
    /// preserving its exact runtime `Intrinsic` identity (`usize`, `isize`, `i8..u32`, `f32`, `f64`, `i64`, `u64`).
    #[inline]
    pub fn try_from_typed_scalar(kind: type_pool::Intrinsic, number: Number) -> Option<Self> {
        use type_pool::Intrinsic;
        match (kind, number) {
            (Intrinsic::I64, Number::I64(v)) => Self::try_from_i64(v),
            (Intrinsic::U64, Number::U64(v)) => Self::try_from_u64(v),
            (Intrinsic::F64, Number::F64(v)) => Self::try_from_f64(v),
            (Intrinsic::Usize, Number::U64(v)) => Self::try_from_usize(v),
            (Intrinsic::Isize, Number::I64(v)) => Self::try_from_isize(v),
            (Intrinsic::U8, Number::U64(v)) if v <= u8::MAX as u64 => {
                Some(Self::make_imm(IMM_NARROW, (NARROW_U8 << 32) | v))
            }
            (Intrinsic::U16, Number::U64(v)) if v <= u16::MAX as u64 => {
                Some(Self::make_imm(IMM_NARROW, (NARROW_U16 << 32) | v))
            }
            (Intrinsic::U32, Number::U64(v)) if v <= u32::MAX as u64 => {
                Some(Self::make_imm(IMM_NARROW, (NARROW_U32 << 32) | v))
            }
            (Intrinsic::I8, Number::I64(v)) if v >= i8::MIN as i64 && v <= i8::MAX as i64 => {
                let raw32 = (v as i32 as u32) as u64;
                Some(Self::make_imm(IMM_NARROW, (NARROW_I8 << 32) | raw32))
            }
            (Intrinsic::I16, Number::I64(v)) if v >= i16::MIN as i64 && v <= i16::MAX as i64 => {
                let raw32 = (v as i32 as u32) as u64;
                Some(Self::make_imm(IMM_NARROW, (NARROW_I16 << 32) | raw32))
            }
            (Intrinsic::I32, Number::I64(v)) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => {
                let raw32 = (v as i32 as u32) as u64;
                Some(Self::make_imm(IMM_NARROW, (NARROW_I32 << 32) | raw32))
            }
            (Intrinsic::F32, Number::F64(v)) => {
                let v32 = v as f32;
                if !v.is_nan() && f64::from(v32) != v {
                    return None;
                }
                let raw32 = v32.to_bits() as u64;
                Some(Self::make_imm(IMM_NARROW, (NARROW_F32 << 32) | raw32))
            }
            _ => None,
        }
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

    /// Decode enum representation only; non-enum immediates are rejected.
    #[inline]
    pub fn as_enum(self) -> Option<(TypeIndex, u32)> {
        if !self.has_imm_sub(IMM_ENUM) {
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

    #[inline]
    pub fn as_trait_proof(self) -> Option<u64> {
        self.has_imm_sub(IMM_TRAIT_PROOF)
            .then(|| self.imm_data())
            .filter(|&id| id != 0)
    }

    /// Create a HeapObject tagged value from a raw object pointer.
    ///
    /// # Safety
    /// `ptr` must be 8-byte aligned and point to a valid GC-managed object payload.
    #[inline]
    pub unsafe fn from_heap_ptr(ptr: *const u8) -> Self {
        debug_assert!((ptr as u64) & TAG_MASK == 0, "heap pointer not aligned");
        Self(ptr as u64 | TAG_HEAP)
    }

    // -- Tag queries ---------------------------------------------------------

    #[inline]
    pub const fn tag(self) -> u64 {
        self.0 & TAG_MASK
    }

    #[inline]
    pub const fn is_heap(self) -> bool {
        self.tag() == TAG_HEAP && self.0 != 0
    }

    #[inline]
    pub const fn is_immediate(self) -> bool {
        self.tag() != TAG_HEAP
    }

    #[inline]
    const fn imm_sub(self) -> u64 {
        (self.0 & IMM_SUB_MASK) >> IMM_SUB_SHIFT
    }

    #[inline]
    const fn imm_data(self) -> u64 {
        self.0 >> IMM_DATA_SHIFT
    }

    #[inline]
    pub const fn is_null(self) -> bool {
        self.has_imm_sub(IMM_NULL)
    }

    #[inline]
    pub const fn is_unit(self) -> bool {
        self.has_imm_sub(IMM_UNIT)
    }

    // -- Extractors ----------------------------------------------------------

    #[inline]
    fn sign_extend_57(raw: u64) -> i64 {
        ((raw << 7) as i64) >> 7
    }

    #[inline]
    pub fn as_i64(self) -> Option<i64> {
        if self.has_imm_sub(IMM_I_SMALL) {
            Some(Self::sign_extend_57(self.imm_data()))
        } else {
            None
        }
    }

    #[inline]
    pub fn as_u64(self) -> Option<u64> {
        if self.has_imm_sub(IMM_U_SMALL) {
            Some(self.imm_data())
        } else {
            None
        }
    }

    #[inline]
    pub fn as_usize_imm(self) -> Option<u64> {
        if self.has_imm_sub(IMM_USIZE) {
            Some(self.imm_data())
        } else {
            None
        }
    }

    #[inline]
    pub fn as_isize_imm(self) -> Option<i64> {
        if self.has_imm_sub(IMM_ISIZE) {
            Some(Self::sign_extend_57(self.imm_data()))
        } else {
            None
        }
    }

    #[inline]
    pub fn as_f64(self) -> Option<f64> {
        if (self.0 & ROT_F64_TAG_MASK) != 0 {
            Some(f64::from_bits(self.0.rotate_right(4)))
        } else if self.has_imm_sub(IMM_F64) {
            Some(f64::from_bits(self.0 & !IMM_LOW7_MASK))
        } else {
            None
        }
    }

    #[inline]
    pub fn as_bool(self) -> Option<bool> {
        if self.has_imm_sub(IMM_BOOL) {
            Some(self.imm_data() != 0)
        } else {
            None
        }
    }

    #[inline]
    pub fn as_char(self) -> Option<char> {
        if self.has_imm_sub(IMM_CHAR) {
            char::from_u32(self.imm_data() as u32)
        } else {
            None
        }
    }

    /// Decode an `IMM_NARROW` scalar into `(Intrinsic, Number)`, validating exact domain bounds.
    #[inline]
    pub fn as_narrow_scalar(self) -> Option<(type_pool::Intrinsic, Number)> {
        use type_pool::Intrinsic;
        if !self.has_imm_sub(IMM_NARROW) {
            return None;
        }
        let data = self.imm_data();
        if (data >> 36) != 0 {
            return None;
        }
        let kind = (data >> 32) & 0xF;
        let raw32 = (data & 0xFFFF_FFFF) as u32;
        match kind {
            NARROW_U8 if raw32 <= u8::MAX as u32 => {
                Some((Intrinsic::U8, Number::U64(raw32 as u64)))
            }
            NARROW_U16 if raw32 <= u16::MAX as u32 => {
                Some((Intrinsic::U16, Number::U64(raw32 as u64)))
            }
            NARROW_U32 => Some((Intrinsic::U32, Number::U64(raw32 as u64))),
            NARROW_I8 => {
                let s = raw32 as i32;
                (s >= i8::MIN as i32 && s <= i8::MAX as i32)
                    .then_some((Intrinsic::I8, Number::I64(s as i64)))
            }
            NARROW_I16 => {
                let s = raw32 as i32;
                (s >= i16::MIN as i32 && s <= i16::MAX as i32)
                    .then_some((Intrinsic::I16, Number::I64(s as i64)))
            }
            NARROW_I32 => Some((Intrinsic::I32, Number::I64((raw32 as i32) as i64))),
            NARROW_F32 => Some((
                Intrinsic::F32,
                Number::F64(f64::from(f32::from_bits(raw32))),
            )),
            _ => None,
        }
    }

    /// Decode any inline immediate number (`f64`, `i64`, `u64`, `usize`, `isize`, `i8..u32`, `f32`)
    /// in a single tag dispatch without dereferencing heap pointers.
    #[inline]
    pub fn as_inline_number(self) -> Option<Number> {
        if (self.0 & ROT_F64_TAG_MASK) != 0 {
            return Some(Number::F64(f64::from_bits(self.0.rotate_right(4))));
        }
        if self.tag() != TAG_IMM {
            return None;
        }
        match self.imm_sub() {
            IMM_I_SMALL | IMM_ISIZE => Some(Number::I64(Self::sign_extend_57(self.imm_data()))),
            IMM_U_SMALL | IMM_USIZE => Some(Number::U64(self.imm_data())),
            IMM_F64 => Some(Number::F64(f64::from_bits(self.0 & !IMM_LOW7_MASK))),
            IMM_NARROW => self.as_narrow_scalar().map(|(_, n)| n),
            _ => None,
        }
    }

    /// Return the exact runtime [`type_pool::Intrinsic`] for any inline scalar immediate
    /// (`i64`, `u64`, `usize`, `isize`, `i8..u32`, `f32`, `f64`, `bool`, `char`, `Unit`).
    #[inline]
    pub fn fast_scalar_intrinsic(self) -> Option<type_pool::Intrinsic> {
        use type_pool::Intrinsic;
        if (self.0 & ROT_F64_TAG_MASK) != 0 {
            return Some(Intrinsic::F64);
        }
        if self.tag() != TAG_IMM {
            return None;
        }
        match self.imm_sub() {
            IMM_I_SMALL => Some(Intrinsic::I64),
            IMM_U_SMALL => Some(Intrinsic::U64),
            IMM_F64 => Some(Intrinsic::F64),
            IMM_BOOL => Some(Intrinsic::Bool),
            IMM_UNIT => Some(Intrinsic::Unit),
            IMM_CHAR => self.as_char().map(|_| Intrinsic::Char),
            IMM_USIZE => Some(Intrinsic::Usize),
            IMM_ISIZE => Some(Intrinsic::Isize),
            IMM_NARROW => self.as_narrow_scalar().map(|(k, _)| k),
            _ => None,
        }
    }

    /// Decode a type value, rejecting malformed raw payloads and INVALID.
    /// This validates the representation; the VM validates pool membership.
    #[inline]
    pub fn as_type(self) -> Option<TypeIndex> {
        if !self.has_imm_sub(IMM_TYPE) {
            return None;
        }
        let raw = u32::try_from(self.imm_data()).ok()?;
        let ty = TypeIndex::from_raw(raw);
        (ty != TypeIndex::INVALID).then_some(ty)
    }

    /// Get the raw heap pointer (payload pointer, after ObjectHeader).
    #[inline]
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
        } else if let Some(v) = self.as_f64() {
            write!(f, "{v}f")
        } else if let Some(v) = self.as_bool() {
            write!(f, "{v}")
        } else if let Some(v) = self.as_char() {
            write!(f, "'{v}'")
        } else if let Some(ty) = self.as_type() {
            write!(f, "Type({ty:?})")
        } else if let Some((k, n)) = self.as_narrow_scalar() {
            write!(f, "{n}({k:?})")
        } else if let Some(v) = self.as_usize_imm() {
            write!(f, "{v}usize")
        } else if let Some(v) = self.as_isize_imm() {
            write!(f, "{v}isize")
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

/// Entry recorded on a frame's LIFO cleanup stack for deterministic cancellation unwinding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwindEntry {
    /// Resume bytecode at `pc` with `r0 = true` to run a `defer` cleanup block.
    DeferPc(u32),
    /// Cancel and await all child tasks in `scope_id`, then pop the top `EffectHandler`.
    HandlerScope(u64),
}

#[derive(Clone)]
struct SavedRegsBuf {
    entries: [(Reg, TaggedValue); Reg::CALLEE_SAVED.len()],
    len: u8,
}

/// Compact lazy-allocated buffer for callee-saved registers (`Reg::CALLEE_SAVED`).
/// Occupies a single 8-byte null pointer (`None`) with zero heap allocation and zero
/// copy overhead when no callee-saved registers are spilled (100% of compiled functions).
#[derive(Clone, Default)]
pub struct SavedRegs {
    inner: Option<Box<SavedRegsBuf>>,
}

impl SavedRegs {
    pub const fn new() -> Self {
        Self { inner: None }
    }

    pub fn from_slice(items: &[(Reg, TaggedValue)]) -> Self {
        let mut out = Self::new();
        for &(reg, val) in items {
            out.push(reg, val);
        }
        out
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_none()
    }

    #[inline]
    pub fn push(&mut self, reg: Reg, val: TaggedValue) {
        let buf = self.inner.get_or_insert_with(|| {
            Box::new(SavedRegsBuf {
                entries: [(Reg(0), TaggedValue::UNIT); Reg::CALLEE_SAVED.len()],
                len: 0,
            })
        });
        let idx = buf.len as usize;
        assert!(idx < buf.entries.len(), "SavedRegs capacity exceeded");
        buf.entries[idx] = (reg, val);
        buf.len += 1;
    }

    #[inline]
    pub fn as_slice(&self) -> &[(Reg, TaggedValue)] {
        match &self.inner {
            Some(buf) => &buf.entries[..buf.len as usize],
            None => &[],
        }
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [(Reg, TaggedValue)] {
        match &mut self.inner {
            Some(buf) => &mut buf.entries[..buf.len as usize],
            None => &mut [],
        }
    }
}

impl std::ops::Deref for SavedRegs {
    type Target = [(Reg, TaggedValue)];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl std::ops::DerefMut for SavedRegs {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<'a> IntoIterator for &'a SavedRegs {
    type Item = &'a (Reg, TaggedValue);
    type IntoIter = std::slice::Iter<'a, (Reg, TaggedValue)>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a mut SavedRegs {
    type Item = &'a mut (Reg, TaggedValue);
    type IntoIter = std::slice::IterMut<'a, (Reg, TaggedValue)>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

/// A frame on the call stack.
#[derive(Clone)]
pub struct CallFrame {
    pub display_state: Option<DisplayState>,
    /// Return address (instruction index to return to).
    pub return_pc: u32,
    /// Function being executed.
    pub func_id: FuncId,
    /// Saved register values (spilled before call).
    pub saved_regs: SavedRegs,
    /// Caller-local values, moved into the frame while the callee executes.
    pub local_slots: Vec<TaggedValue>,
    /// Saved LIFO cleanup actions (`defer` PCs and handler scopes) of the caller frame.
    pub unwind_stack: Vec<UnwindEntry>,
    /// Whether the caller frame was actively executing a cancellation `defer` block.
    pub is_unwind_target: bool,
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

/// Unique generational identifier for a task: `(generation << 32) | slot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

impl TaskId {
    pub const fn from_parts(slot: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | (slot as u64))
    }

    pub const fn slot(self) -> u32 {
        self.0 as u32
    }

    pub const fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

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
    /// Active or unobserved-failed Future IDs spawned directly by this task.
    pub promise_set: std::collections::BTreeSet<u64>,
    /// If this task was spawned for an async effect handler, the promise it fulfills.
    pub spawned_promise: Option<u64>,
    /// Cooperative/safepoint cancellation flag propagated down the task tree.
    pub cancelled: bool,
    /// True once this task has entered deterministic cancellation `defer`/handler unwinding.
    pub unwinding_cancel: bool,
    /// True while suspended inside `begin_or_step_cancel_unwind` awaiting child cleanup.
    pub paused_in_unwind_step: bool,
    /// Snapshot of visible ancestor tasks' effect handlers at spawn time (outermost to innermost).
    /// Stored per-task so moving GC updates `closure_env` slots without cross-task races.
    pub ancestor_handlers: Vec<EffectHandler>,
}

impl TaskState {
    pub fn new(id: TaskId, func_id: FuncId) -> Self {
        Self::from_stacks(id, TaskStacks::new(func_id))
    }

    pub fn new_with_pool(
        id: TaskId,
        func_id: FuncId,
        pool: std::sync::Arc<stack_pool::StackPool>,
    ) -> Self {
        Self::from_stacks(id, TaskStacks::with_pool(func_id, pool))
    }

    /// Reinitialize a reaped `TaskState` in place for reuse on a new `TaskId` generation.
    pub fn reinit_for_spawn(
        &mut self,
        id: TaskId,
        func_id: FuncId,
        pool: Option<std::sync::Arc<stack_pool::StackPool>>,
    ) {
        self.id = id;
        self.status = TaskStatus::Ready;
        self.stacks.reinit_for_spawn(func_id, pool);
        self.parent = None;
        self.children.clear();
        self.promise_set.clear();
        self.spawned_promise = None;
        self.cancelled = false;
        self.unwinding_cancel = false;
        self.paused_in_unwind_step = false;
        self.ancestor_handlers.clear();
    }

    fn from_stacks(id: TaskId, stacks: TaskStacks) -> Self {
        Self {
            id,
            status: TaskStatus::Ready,
            stacks,
            parent: None,
            children: Vec::new(),
            promise_set: std::collections::BTreeSet::new(),
            spawned_promise: None,
            cancelled: false,
            unwinding_cancel: false,
            paused_in_unwind_step: false,
            ancestor_handlers: Vec::new(),
        }
    }

    /// Snapshot the visible handler chain into a stack-allocated buffer `buf` without
    /// heap allocation whenever `<= N` handlers are visible (returns `None` if `> N`).
    pub fn snapshot_child_ancestor_handlers_inline<const N: usize>(
        &self,
        buf: &mut [EffectHandler; N],
    ) -> Option<usize> {
        let mut own_buf = [EffectHandler::PLACEHOLDER; N];
        let own_len = self
            .stacks
            .collect_effective_handlers_inline(&mut own_buf)?;
        let mut out_len = 0usize;
        for handler in &self.ancestor_handlers {
            if !own_buf[..own_len]
                .iter()
                .any(|inner| inner.effect_type == handler.effect_type)
            {
                if out_len >= N {
                    return None;
                }
                buf[out_len] = *handler;
                out_len += 1;
            }
        }
        if out_len + own_len > N {
            return None;
        }
        buf[out_len..out_len + own_len].copy_from_slice(&own_buf[..own_len]);
        Some(out_len + own_len)
    }

    /// Snapshot the visible handler chain (ancestors + own active segments) for a newly
    /// spawned child task, keeping only the nearest binding per effect type.
    pub fn snapshot_child_ancestor_handlers(&self) -> Vec<EffectHandler> {
        let own_effective = self.stacks.effective_handlers();
        if self.ancestor_handlers.is_empty() {
            return own_effective;
        }
        let mut merged = Vec::with_capacity(self.ancestor_handlers.len() + own_effective.len());
        for handler in &self.ancestor_handlers {
            if !own_effective
                .iter()
                .any(|inner| inner.effect_type == handler.effect_type)
            {
                merged.push(*handler);
            }
        }
        merged.extend(own_effective);
        merged
    }

    /// Find the nearest handler for `effect` in this task's active stack chain, falling
    /// back to `ancestor_handlers`. The boolean is `true` when resolved from an ancestor task.
    pub fn find_handler_with_ancestors(&self, effect: TypeIndex) -> Option<(&EffectHandler, bool)> {
        if let Some(local) = self.stacks.find_handler(effect) {
            return Some((local, false));
        }
        self.ancestor_handlers
            .iter()
            .rev()
            .find(|handler| handler.effect_type == effect)
            .map(|handler| (handler, true))
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
#[derive(Debug, Clone, Copy)]
pub struct EffectHandler {
    pub effect_type: TypeIndex,
    /// Function to call when the effect is invoked.
    pub handler_func: FuncId,
    /// Closure and its captures, retained as a GC root while installed.
    pub closure_env: TaggedValue,
    /// Runtime-provided continuation argument position for capturing handlers.
    pub continuation_param: Option<u8>,
    /// Unique identifier for this handler's lexical installation scope.
    pub scope_id: u64,
}

impl EffectHandler {
    pub const PLACEHOLDER: Self = Self {
        effect_type: TypeIndex::INVALID,
        handler_func: FuncId(0),
        closure_env: TaggedValue::UNIT,
        continuation_param: None,
        scope_id: 0,
    };
}

// ---------------------------------------------------------------------------
// BytecodeStore — holds all compiled function bytecode
// ---------------------------------------------------------------------------

/// Precomputed per-function execution metadata derived once at `add_function` time.
pub struct FunctionExecMeta {
    pub decoded: Box<[Option<nsbc::Instruction>]>,
    pub clobbers_callee_saved: bool,
}

impl FunctionExecMeta {
    fn from_code(code: &FunctionCode) -> Self {
        let mut decoded = Vec::with_capacity(code.instructions.len());
        let mut clobbers_callee_saved = false;
        for &word in &code.instructions {
            let instr = nsbc::Instruction::decode(word);
            if let Some(ref ins) = instr {
                let check_reg = |r: Reg| (19..=28).contains(&r.0);
                match ins.data {
                    nsbc::InstructionData::R { dst, src1, .. }
                        if check_reg(dst)
                            || (ins.opcode == nsbc::Opcode::Swap && check_reg(src1)) =>
                    {
                        clobbers_callee_saved = true;
                    }
                    nsbc::InstructionData::A { dst, .. }
                        if ins.opcode != nsbc::Opcode::StoreSlot
                            && ins.opcode != nsbc::Opcode::StoreField
                            && ins.opcode != nsbc::Opcode::StoreGlobal
                            && ins.opcode != nsbc::Opcode::StoreGlobalWide
                            && check_reg(dst) =>
                    {
                        clobbers_callee_saved = true;
                    }
                    _ => {}
                }
            }
            decoded.push(instr);
        }
        Self {
            decoded: decoded.into_boxed_slice(),
            clobbers_callee_saved,
        }
    }
}

/// Stores compiled bytecode for all functions in the program.
pub struct BytecodeStore {
    /// Per-function instruction sequences, indexed by FuncId.
    functions: Vec<FunctionCode>,
    meta: Vec<FunctionExecMeta>,
}

pub struct FunctionCode {
    pub display_owner: Option<type_pool::TypeIndex>,
    pub func_id: FuncId,
    pub instructions: Vec<u32>, // encoded 32-bit instruction words
    pub register_count: u8,
    pub param_count: u8,
    pub is_closure: bool,
    pub function_type: type_pool::TypeIndex,
    /// Mandatory physical entry layout separating captures and parameters.
    pub abi: nsbc::FunctionAbi,
}

impl BytecodeStore {
    pub fn new() -> Self {
        Self {
            functions: Vec::new(),
            meta: Vec::new(),
        }
    }

    pub fn add_function(&mut self, code: FunctionCode) -> FuncId {
        let id = FuncId(self.functions.len() as u32);
        debug_assert_eq!(code.func_id, id);
        let exec_meta = FunctionExecMeta::from_code(&code);
        self.functions.push(code);
        self.meta.push(exec_meta);
        id
    }

    #[inline]
    pub fn get_function(&self, id: FuncId) -> &FunctionCode {
        &self.functions[id.0 as usize]
    }

    #[inline]
    pub fn get_meta(&self, id: FuncId) -> &FunctionExecMeta {
        &self.meta[id.0 as usize]
    }

    #[inline]
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

const GLOBAL_STATE_UNINIT: u8 = 0;
const GLOBAL_STATE_WRITING_CONST: u8 = 1;
const GLOBAL_STATE_INIT: u8 = 2;

/// Thread-safe table of runtime global variables.
///
/// Reads and stores use Acquire/Release atomic operations on fixed-capacity slices
/// installed before execution, while stopped-world GC root scanning updates raw slots
/// exclusively via `visit_raw_slots_mut`.
pub struct GlobalTable {
    values: Vec<std::sync::atomic::AtomicU64>,
    slots: Vec<GlobalSlot>,
    schema_initialized: bool,
}

struct GlobalSlot {
    info: GlobalInfo,
    state: std::sync::atomic::AtomicU8,
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
        self.values.extend(
            (0..schema.len()).map(|_| std::sync::atomic::AtomicU64::new(TaggedValue::UNIT.raw())),
        );
        self.slots.extend(schema.iter().map(|&info| GlobalSlot {
            info,
            state: std::sync::atomic::AtomicU8::new(GLOBAL_STATE_UNINIT),
        }));
        self.schema_initialized = true;
        Ok(())
    }

    /// Incrementally append module globals during per-module dynamic loading outside `Vm::run()`.
    /// Returns the starting global index (`global_base`) assigned to `schema[0]`.
    pub fn extend_schema(&mut self, schema: &[GlobalInfo]) -> Result<u32, GlobalError> {
        let base = u32::try_from(self.values.len()).map_err(|_| GlobalError::OutOfMemory)?;
        if schema.is_empty() {
            return Ok(base);
        }
        self.values
            .try_reserve(schema.len())
            .map_err(|_| GlobalError::OutOfMemory)?;
        self.slots
            .try_reserve(schema.len())
            .map_err(|_| GlobalError::OutOfMemory)?;
        self.values.extend(
            (0..schema.len()).map(|_| std::sync::atomic::AtomicU64::new(TaggedValue::UNIT.raw())),
        );
        self.slots.extend(schema.iter().map(|&info| GlobalSlot {
            info,
            state: std::sync::atomic::AtomicU8::new(GLOBAL_STATE_UNINIT),
        }));
        self.schema_initialized = true;
        Ok(base)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn alloc(&mut self, init: TaggedValue) -> u32 {
        let idx = self.values.len() as u32;
        self.values
            .push(std::sync::atomic::AtomicU64::new(init.raw()));
        self.slots.push(GlobalSlot {
            info: GlobalInfo {
                type_index: type_pool::Intrinsic::Any.type_index(),
                is_mutable: true,
            },
            state: std::sync::atomic::AtomicU8::new(GLOBAL_STATE_INIT),
        });
        idx
    }

    pub fn get(&self, idx: u32) -> Result<TaggedValue, GlobalError> {
        use std::sync::atomic::Ordering;
        let slot = self
            .slots
            .get(idx as usize)
            .ok_or(GlobalError::InvalidIndex(idx))?;
        if slot.state.load(Ordering::Acquire) != GLOBAL_STATE_INIT {
            return Err(GlobalError::Uninitialized(idx));
        }
        Ok(TaggedValue::from_raw(
            self.values[idx as usize].load(Ordering::Acquire),
        ))
    }

    pub fn set(&self, idx: u32, val: TaggedValue) -> Result<(), GlobalError> {
        use std::sync::atomic::Ordering;
        let slot = self
            .slots
            .get(idx as usize)
            .ok_or(GlobalError::InvalidIndex(idx))?;
        if slot.info.is_mutable {
            self.values[idx as usize].store(val.raw(), Ordering::Release);
            slot.state.store(GLOBAL_STATE_INIT, Ordering::Release);
            Ok(())
        } else {
            slot.state
                .compare_exchange(
                    GLOBAL_STATE_UNINIT,
                    GLOBAL_STATE_WRITING_CONST,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map_err(|_| GlobalError::Immutable(idx))?;
            self.values[idx as usize].store(val.raw(), Ordering::Release);
            slot.state.store(GLOBAL_STATE_INIT, Ordering::Release);
            Ok(())
        }
    }

    /// Check write permission before any potentially allocating type coercion.
    pub fn check_store(&self, idx: u32) -> Result<GlobalInfo, GlobalError> {
        use std::sync::atomic::Ordering;
        let slot = self
            .slots
            .get(idx as usize)
            .ok_or(GlobalError::InvalidIndex(idx))?;
        if !slot.info.is_mutable && slot.state.load(Ordering::Acquire) != GLOBAL_STATE_UNINIT {
            return Err(GlobalError::Immutable(idx));
        }
        Ok(slot.info)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Snapshot current global values in slot order (used by tests and inspection).
    pub fn values_snapshot(&self) -> Vec<TaggedValue> {
        use std::sync::atomic::Ordering;
        self.values
            .iter()
            .map(|atom| TaggedValue::from_raw(atom.load(Ordering::Acquire)))
            .collect()
    }

    /// Visit registered raw value slots during exclusive stopped-world root tracing.
    /// Forwarding preserves each slot's type and initialization state.
    pub fn visit_raw_slots_mut(&mut self, mut visit: impl FnMut(*mut u64)) {
        for atom in &mut self.values {
            visit(std::ptr::from_mut(atom.get_mut()));
        }
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
        assert_eq!(
            table.values_snapshot(),
            vec![TaggedValue::UNIT, TaggedValue::TRUE]
        );
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

    pub fn ensure_len(&mut self, min_len: usize) {
        if self.values.len() < min_len {
            self.values.resize(min_len, TaggedValue::UNIT);
        }
    }

    pub fn set(&mut self, idx: u32, val: TaggedValue) {
        let pos = idx as usize;
        if self.values.len() <= pos {
            self.values.resize(pos + 1, TaggedValue::UNIT);
        }
        self.values[pos] = val;
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
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
    fn enum_immediates_preserve_identity_and_reject_other_immediates() {
        for ty in [0, 1, u32::MAX - 1] {
            for variant in [0, (1 << 25) - 1] {
                let ty = TypeIndex::from_raw(ty);
                let value = TaggedValue::from_enum(ty, variant).unwrap();
                assert_eq!(value.as_enum(), Some((ty, variant)));
                assert_eq!(value.as_type(), None);
                assert_eq!(TaggedValue::from_u64(value.imm_data()).as_enum(), None);
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
