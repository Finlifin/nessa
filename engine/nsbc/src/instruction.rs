//! NSBC instruction set definitions.
//!
//! Fixed 32-bit instruction encoding with format families:
//! R-type (register-register), A-type (addressed operand),
//! J-type (jump), C-type (call/return), E-type (effect/system).
//!
//! Bits [31:24] opcode, [23:22] addressing mode, [21:0] payload.

use type_pool::TypeIndex;

// ---------------------------------------------------------------------------
// Register and FuncId newtypes
// ---------------------------------------------------------------------------

/// A register number (0..31 GP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u8);

impl Reg {
    pub const MAX_GP: u8 = 32;

    // ABI (ARM-inspired)
    pub const ARG0: u8 = 0;
    pub const SCRATCH0: u8 = 16;
    pub const SCRATCH1: u8 = 17;
    pub const RESERVED_PLATFORM: u8 = 18;
    pub const CALLEE_SAVE_FIRST: u8 = 19;
    pub const CALLEE_SAVE_LAST: u8 = 28;
    pub const RESERVED_FP: u8 = 29;
    pub const RESERVED_LR: u8 = 30;
    pub const RESERVED_SP: u8 = 31;

    /// Callee-saved registers r19..=r28 that the callee (VM) must preserve across calls.
    pub const CALLEE_SAVED: &'static [u8] = &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28];
}

/// Identifies a function in the bytecode store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

// ---------------------------------------------------------------------------
// Addressing mode
// ---------------------------------------------------------------------------

/// 2-bit addressing mode in bits [23:22].
///
/// Only A-type instructions that load a general operand interpret `amode`.
/// Other formats encode `Imm` (00) and ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AddrMode {
    /// Operand is the (sign- or zero-extended) immediate field.
    Imm = 0b00,
    /// Operand is `const_pool[imm]`.
    Const = 0b01,
    /// Operand is `r[base] + sext(offset)` (SCALE = 0).
    RegOff = 0b10,
    /// Reserved — decode fails.
    Reserved = 0b11,
}

impl AddrMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v & 0b11 {
            0b00 => Some(Self::Imm),
            0b01 => Some(Self::Const),
            0b10 => Some(Self::RegOff),
            _ => None, // reserved → None so decode can reject
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

// ---------------------------------------------------------------------------
// Opcode — all bytecode operations
// ---------------------------------------------------------------------------

/// All opcodes for the NSBC instruction set.
///
/// High 2 bits select the format family:
/// - `00` R-type
/// - `01` A-type (addressed / indexed)
/// - `10` J-type (jumps) or C-type (calls) — distinguished by opcode range
/// - `11` E-type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Opcode {
    // ── R-type (00): arithmetic & register ops ─────────────────
    Add = 0x00,
    Sub = 0x01,
    Mul = 0x02,
    Div = 0x03,
    Mod = 0x04,
    Neg = 0x05,
    BitAnd = 0x06,
    BitOr = 0x07,
    BitXor = 0x08,
    BitNot = 0x09,
    Shl = 0x0A,
    Shr = 0x0B,
    UShr = 0x0C,

    // ── R-type (00): comparison ────────────────────────────────
    CmpEq = 0x10,
    CmpNe = 0x11,
    CmpLt = 0x12,
    CmpLe = 0x13,
    CmpGt = 0x14,
    CmpGe = 0x15,

    // ── R-type (00): register manipulation ─────────────────────
    Mov = 0x18,
    Swap = 0x19,

    // ── A-type (01): loads with addressing mode ────────────────
    /// `r[dst] = operand(amode)` — Imm / Const / RegOff.
    Load = 0x40,
    /// Wide constant-pool load: index in imm12|base<<12 (19-bit), amode ignored.
    LoadConstWide = 0x41,
    LoadUnit = 0x42,
    LoadTrue = 0x43,
    LoadFalse = 0x44,
    LoadNull = 0x45,
    TypeCheck = 0x48,
    TypeCast = 0x49,
    TypeCastSafe = 0x4A,
    TypeAssert = 0x4B,

    // ── A-type (01): memory / index (independent of amode) ─────
    LoadField = 0x50,
    StoreField = 0x51,
    LoadIndex = 0x52,
    StoreIndex = 0x53,
    LoadGlobal = 0x54,
    StoreGlobal = 0x55,
    LoadCapture = 0x56,
    LoadGlobalWide = 0x57,
    StoreGlobalWide = 0x58,

    // ── A-type (01): object creation ───────────────────────────
    NewObject = 0x60,
    NewList = 0x61,
    NewMap = 0x62,
    NewClosure = 0x63,
    NewClosureWide = 0x64,
    LoadSlot = 0x65,
    StoreSlot = 0x66,
    NewEnum = 0x67,
    EnumIs = 0x68,
    EnumField = 0x69,
    /// Obtain a caller-selected trait proof for raw receiver data.
    TraitProof = 0x6A,
    /// Check data in dst against the proof in base and the declared view.
    TraitAssert = 0x6B,
    /// Project a frozen proof to an ancestor trait view.
    TraitProject = 0x6C,
    ErrorOk = 0x6D,
    ErrorErr = 0x6E,
    ErrorIsOk = 0x6F,
    ErrorPayload = 0x70,

    // ── J-type (10): control flow ──────────────────────────────
    Jmp = 0x80,
    JmpIf = 0x81,
    JmpIfNot = 0x82,
    JmpIfNull = 0x83,
    JmpIfNotNull = 0x84,
    /// Far jump: 22-bit signed offset in payload (amode ignored).
    JmpFar = 0x85,

    // ── C-type (10): calls / returns ───────────────────────────
    Call = 0x88,
    CallIndirect = 0x89,
    CallMethod = 0x8A,
    CallWasm = 0x8B,
    TailCall = 0x8C,
    /// Call a registered builtin by [`runtime::BuiltinFnId`](id).
    CallBuiltin = 0x8D,
    ReturnUnit = 0x8E,
    Return = 0x8F,
    /// Far call: func_id from const pool index in imm12, arg_count in high bits.
    CallFar = 0x90,
    CallMethodFar = 0x91,
    /// Frozen proof in receiver field, interface slot in imm12.
    TraitCall = 0x92,
    /// Indirect call whose arguments already use the physical user layout.
    CallIndirectProof = 0x93,

    // ── E-type (11): effects & continuations ───────────────────
    EffectCall = 0xC0,
    EffectCallDyn = 0xC1,
    PushHandler = 0xC2,
    PopHandler = 0xC3,
    Shift = 0xC4,
    Reset = 0xC5,
    Resume = 0xC6,
    PushHandlerWide = 0xC7,
    CloneContinuation = 0xC8,
    DropContinuation = 0xC9,
    PushHandlerClosure = 0xCA,
    PushCapturingHandler = 0xCB,
    ResetClosure = 0xCC,
    ResumeContinuation = 0xCD,
    ResumeContinuationOnce = 0xCE,

    // ── E-type (11): system ────────────────────────────────────
    Safepoint = 0xD0,
    DebugBreak = 0xD1,
    Nop = 0xD2,
    AllocateSlots = 0xD3,
    MatchFail = 0xD4,
}

impl Opcode {
    pub const fn format(self) -> Format {
        match self as u8 {
            0x00..=0x3F => Format::R,
            0x40..=0x7F => Format::A,
            0x80..=0x87 => Format::J,
            0x88..=0xBF => Format::C,
            _ => Format::E,
        }
    }

    /// Whether this A-type opcode interprets the addressing-mode field.
    pub const fn uses_amode(self) -> bool {
        matches!(self, Opcode::Load)
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        if !matches!(v,
            0x00..=0x0C | 0x10..=0x15 | 0x18..=0x19 | 0x40..=0x45 |
            0x48..=0x4B | 0x50..=0x58 | 0x60..=0x70 | 0x80..=0x85 |
            0x88..=0x93 | 0xC0..=0xCE | 0xD0..=0xD4
        ) {
            return None;
        }
        // SAFETY: Opcode is repr(u8), and every value in these checked ranges
        // has an explicitly declared variant. Gaps are rejected above.
        Some(unsafe { std::mem::transmute::<u8, Self>(v) })
    }
}

// ---------------------------------------------------------------------------
// Instruction format
// ---------------------------------------------------------------------------

/// Instruction format family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// R-type: dst(5) src1(5) src2(5)
    R,
    /// A-type: dst(5) base/src(5) imm12(12) + amode
    A,
    /// J-type: cond(5) offset(17)
    J,
    /// C-type: call/return payloads
    C,
    /// E-type: payload(22)
    E,
}

// ---------------------------------------------------------------------------
// Instruction — a decoded 32-bit instruction
// ---------------------------------------------------------------------------

/// A single decoded NSBC instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    pub opcode: Opcode,
    pub amode: AddrMode,
    pub data: InstructionData,
}

/// The decoded operands, varying by format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionData {
    R {
        dst: Reg,
        src1: Reg,
        src2: Reg,
    },
    A {
        dst: Reg,
        /// Base register for RegOff, value src for Type*, capture_count for NewClosure, etc.
        base: Reg,
        /// 12-bit immediate / index / signed offset (stored as u16, low 12 bits).
        imm12: u16,
    },
    J {
        cond: Reg,
        /// 17-bit signed PC-relative offset (sign-extended into i32).
        offset: i32,
    },
    C {
        /// Opaque 22-bit payload; interpreted per opcode.
        payload: u32,
    },
    E {
        /// 22-bit payload.
        payload: u32,
    },
}

// Field widths / masks
const IMM12_MASK: u32 = 0xFFF;
const OFFSET17_MASK: u32 = 0x1_FFFF;
const PAYLOAD22_MASK: u32 = 0x3F_FFFF;

fn sign_extend(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

// ---------------------------------------------------------------------------
// Encoding / decoding
// ---------------------------------------------------------------------------

impl Instruction {
    /// Encode to a 32-bit word.
    pub fn encode(self) -> u32 {
        let op = self.opcode as u32;
        let am = (self.amode.as_u8() as u32) & 0b11;
        let payload = match self.data {
            InstructionData::R { dst, src1, src2 } => {
                ((dst.0 as u32 & 0x1F) << 17)
                    | ((src1.0 as u32 & 0x1F) << 12)
                    | ((src2.0 as u32 & 0x1F) << 7)
            }
            InstructionData::A { dst, base, imm12 } => {
                ((dst.0 as u32 & 0x1F) << 17)
                    | ((base.0 as u32 & 0x1F) << 12)
                    | (imm12 as u32 & IMM12_MASK)
            }
            InstructionData::J { cond, offset } => {
                if self.opcode == Opcode::JmpFar {
                    (offset as u32) & PAYLOAD22_MASK
                } else {
                    let off_bits = (offset as u32) & OFFSET17_MASK;
                    ((cond.0 as u32 & 0x1F) << 17) | off_bits
                }
            }
            InstructionData::C { payload } | InstructionData::E { payload } => {
                payload & PAYLOAD22_MASK
            }
        };
        (op << 24) | (am << 22) | payload
    }

    /// Decode from a 32-bit word.  Returns `None` if the opcode is unknown
    /// or if an A-type instruction that uses amode has a reserved mode.
    pub fn decode(word: u32) -> Option<Self> {
        let op_byte = (word >> 24) as u8;
        let opcode = Opcode::from_u8(op_byte)?;
        let am_raw = ((word >> 22) & 0b11) as u8;
        let amode = if opcode.uses_amode() {
            AddrMode::from_u8(am_raw)?
        } else {
            // Non-amode instructions ignore the field; treat reserved as Imm for roundtrip.
            AddrMode::from_u8(am_raw).unwrap_or(AddrMode::Imm)
        };
        let low = word & PAYLOAD22_MASK;
        let data = match opcode.format() {
            Format::R => InstructionData::R {
                dst: Reg(((low >> 17) & 0x1F) as u8),
                src1: Reg(((low >> 12) & 0x1F) as u8),
                src2: Reg(((low >> 7) & 0x1F) as u8),
            },
            Format::A => InstructionData::A {
                dst: Reg(((low >> 17) & 0x1F) as u8),
                base: Reg(((low >> 12) & 0x1F) as u8),
                imm12: (low & IMM12_MASK) as u16,
            },
            Format::J => {
                if opcode == Opcode::JmpFar {
                    // JmpFar: full 22-bit signed offset in payload.
                    InstructionData::J {
                        cond: Reg(0),
                        offset: sign_extend(low, 22),
                    }
                } else {
                    let cond = Reg(((low >> 17) & 0x1F) as u8);
                    let raw_offset = low & OFFSET17_MASK;
                    InstructionData::J {
                        cond,
                        offset: sign_extend(raw_offset, 17),
                    }
                }
            }
            Format::C => InstructionData::C { payload: low },
            Format::E => InstructionData::E { payload: low },
        };
        Some(Instruction {
            opcode,
            amode,
            data,
        })
    }
}

// ---------------------------------------------------------------------------
// Convenient constructors
// ---------------------------------------------------------------------------

impl Instruction {
    pub fn r_type(opcode: Opcode, dst: Reg, src1: Reg, src2: Reg) -> Self {
        Self {
            opcode,
            amode: AddrMode::Imm,
            data: InstructionData::R { dst, src1, src2 },
        }
    }

    pub fn a_type(opcode: Opcode, amode: AddrMode, dst: Reg, base: Reg, imm12: u16) -> Self {
        Self {
            opcode,
            amode,
            data: InstructionData::A {
                dst,
                base,
                imm12: imm12 & 0xFFF,
            },
        }
    }

    pub fn j_type(opcode: Opcode, cond: Reg, offset: i32) -> Self {
        Self {
            opcode,
            amode: AddrMode::Imm,
            data: InstructionData::J { cond, offset },
        }
    }

    pub fn c_type(opcode: Opcode, payload: u32) -> Self {
        Self {
            opcode,
            amode: AddrMode::Imm,
            data: InstructionData::C {
                payload: payload & PAYLOAD22_MASK,
            },
        }
    }

    pub fn e_type(opcode: Opcode, payload: u32) -> Self {
        Self {
            opcode,
            amode: AddrMode::Imm,
            data: InstructionData::E {
                payload: payload & PAYLOAD22_MASK,
            },
        }
    }

    // -- Commonly used shorthand constructors --

    pub fn add(dst: Reg, src1: Reg, src2: Reg) -> Self {
        Self::r_type(Opcode::Add, dst, src1, src2)
    }

    pub fn sub(dst: Reg, src1: Reg, src2: Reg) -> Self {
        Self::r_type(Opcode::Sub, dst, src1, src2)
    }

    pub fn mov(dst: Reg, src: Reg) -> Self {
        Self::r_type(Opcode::Mov, dst, src, Reg(0))
    }

    /// Load a small immediate (fits in signed/unsigned 12-bit as stored).
    pub fn load_imm(dst: Reg, imm: u16) -> Self {
        Self::a_type(Opcode::Load, AddrMode::Imm, dst, Reg(0), imm)
    }

    /// Load from constant pool (index must fit in 12 bits).
    pub fn load_const(dst: Reg, const_idx: u16) -> Self {
        Self::a_type(Opcode::Load, AddrMode::Const, dst, Reg(0), const_idx)
    }

    /// Load `r[base] + sext(offset)`.
    pub fn load_reg_off(dst: Reg, base: Reg, offset: i16) -> Self {
        Self::a_type(
            Opcode::Load,
            AddrMode::RegOff,
            dst,
            base,
            (offset as u16) & 0xFFF,
        )
    }

    /// Wide constant-pool load: 19-bit index = (base<<12) | imm12.
    pub fn load_const_wide(dst: Reg, const_idx: u32) -> Self {
        let imm12 = (const_idx & 0xFFF) as u16;
        let base = Reg(((const_idx >> 12) & 0x1F) as u8);
        Self::a_type(Opcode::LoadConstWide, AddrMode::Imm, dst, base, imm12)
    }

    pub fn load_unit(dst: Reg) -> Self {
        Self::a_type(Opcode::LoadUnit, AddrMode::Imm, dst, Reg(0), 0)
    }

    /// Allocate the current function's local value slots at entry.
    pub fn allocate_slots(count: u32) -> Self {
        assert!(count <= (1 << 17));
        Self::e_type(Opcode::AllocateSlots, count)
    }

    /// Slots use a register field and a 17-bit unsigned slot index.
    pub fn load_slot(dst: Reg, slot: u32) -> Self {
        assert!(slot < (1 << 17));
        Self::a_type(
            Opcode::LoadSlot,
            AddrMode::Imm,
            dst,
            Reg((slot >> 12) as u8),
            (slot & 0xFFF) as u16,
        )
    }

    pub fn store_slot(slot: u32, source: Reg) -> Self {
        assert!(slot < (1 << 17));
        Self::a_type(
            Opcode::StoreSlot,
            AddrMode::Imm,
            source,
            Reg((slot >> 12) as u8),
            (slot & 0xFFF) as u16,
        )
    }

    pub fn load_true(dst: Reg) -> Self {
        Self::a_type(Opcode::LoadTrue, AddrMode::Imm, dst, Reg(0), 0)
    }

    pub fn load_false(dst: Reg) -> Self {
        Self::a_type(Opcode::LoadFalse, AddrMode::Imm, dst, Reg(0), 0)
    }

    pub fn load_null(dst: Reg) -> Self {
        Self::a_type(Opcode::LoadNull, AddrMode::Imm, dst, Reg(0), 0)
    }

    pub fn jmp(offset: i32) -> Self {
        Self::j_type(Opcode::Jmp, Reg(0), offset)
    }

    pub fn jmp_if(cond: Reg, offset: i32) -> Self {
        Self::j_type(Opcode::JmpIf, cond, offset)
    }

    pub fn jmp_if_not(cond: Reg, offset: i32) -> Self {
        Self::j_type(Opcode::JmpIfNot, cond, offset)
    }

    pub fn jmp_far(offset: i32) -> Self {
        Self {
            opcode: Opcode::JmpFar,
            amode: AddrMode::Imm,
            data: InstructionData::J {
                cond: Reg(0),
                offset,
            },
        }
    }

    /// Call with 14-bit func_id and 8-bit arg_count.
    pub fn call(func_id: u32, arg_count: u8) -> Self {
        debug_assert!(func_id < (1 << 14));
        let payload = ((arg_count as u32) << 14) | (func_id & 0x3FFF);
        Self::c_type(Opcode::Call, payload)
    }

    pub fn call_far(const_idx: u16, arg_count: u8) -> Self {
        let payload = ((arg_count as u32) << 14) | (const_idx as u32 & 0x3FFF);
        Self::c_type(Opcode::CallFar, payload)
    }

    pub fn call_builtin(builtin_id: u32, arg_count: u8) -> Self {
        let payload = ((arg_count as u32) << 14) | (builtin_id & 0x3FFF);
        Self::c_type(Opcode::CallBuiltin, payload)
    }

    pub fn ret(src: Reg) -> Self {
        let payload = (src.0 as u32 & 0x1F) << 17;
        Self::c_type(Opcode::Return, payload)
    }

    pub fn return_unit() -> Self {
        Self::c_type(Opcode::ReturnUnit, 0)
    }

    /// CallIndirect: arg_count[21:14], closure_reg[13:9].
    pub fn call_indirect(closure_reg: Reg, arg_count: u8) -> Self {
        let payload = ((arg_count as u32) << 14) | ((closure_reg.0 as u32 & 0x1F) << 9);
        Self::c_type(Opcode::CallIndirect, payload)
    }

    pub fn call_indirect_proof(closure_reg: Reg, physical_arg_count: u8) -> Self {
        let mut instruction = Self::call_indirect(closure_reg, physical_arg_count);
        instruction.opcode = Opcode::CallIndirectProof;
        instruction
    }

    pub fn trait_proof(dst: Reg, receiver: Reg, view: TypeIndex) -> Self {
        debug_assert!(view.as_u32() < 1 << 12);
        Self::a_type(
            Opcode::TraitProof,
            AddrMode::Imm,
            dst,
            receiver,
            view.as_u32() as u16,
        )
    }

    pub fn trait_assert(data: Reg, proof: Reg, view: TypeIndex) -> Self {
        debug_assert!(view.as_u32() < 1 << 12);
        Self::a_type(
            Opcode::TraitAssert,
            AddrMode::Imm,
            data,
            proof,
            view.as_u32() as u16,
        )
    }

    pub fn trait_project(dst: Reg, proof: Reg, view: TypeIndex) -> Self {
        debug_assert!(view.as_u32() < 1 << 12);
        Self::a_type(
            Opcode::TraitProject,
            AddrMode::Imm,
            dst,
            proof,
            view.as_u32() as u16,
        )
    }

    pub fn trait_call(proof: Reg, slot: u32, physical_arg_count: u8) -> Self {
        let mut instruction = Self::call_method(proof, slot, physical_arg_count);
        instruction.opcode = Opcode::TraitCall;
        instruction
    }

    /// CallMethod: arg_count[21:17], recv[16:12], method_id[11:0].
    pub fn call_method(receiver_reg: Reg, method_str_id: u32, arg_count: u8) -> Self {
        debug_assert!(method_str_id < (1 << 12));
        debug_assert!(arg_count < 32);
        let payload = ((arg_count as u32 & 0x1F) << 17)
            | ((receiver_reg.0 as u32 & 0x1F) << 12)
            | (method_str_id & IMM12_MASK);
        Self::c_type(Opcode::CallMethod, payload)
    }

    pub fn call_method_far(receiver_reg: Reg, method_const_idx: u16, arg_count: u8) -> Self {
        debug_assert!(arg_count < 32);
        let payload = ((arg_count as u32 & 0x1F) << 17)
            | ((receiver_reg.0 as u32 & 0x1F) << 12)
            | (method_const_idx as u32 & IMM12_MASK);
        Self::c_type(Opcode::CallMethodFar, payload)
    }

    pub fn tail_call(func_id: u32, arg_count: u8) -> Self {
        debug_assert!(func_id < (1 << 14));
        let payload = ((arg_count as u32) << 14) | (func_id & 0x3FFF);
        Self::c_type(Opcode::TailCall, payload)
    }

    pub fn new_object(dst: Reg, type_idx: TypeIndex) -> Self {
        let idx = type_idx.as_u32();
        debug_assert!(idx < (1 << 12));
        Self::a_type(Opcode::NewObject, AddrMode::Imm, dst, Reg(0), idx as u16)
    }

    /// Allocate a payload variant from a checked tuple of field values.
    pub fn new_enum(dst: Reg, tuple: Reg, descriptor: u16) -> Self {
        Self::a_type(Opcode::NewEnum, AddrMode::Imm, dst, tuple, descriptor)
    }

    pub fn enum_is(dst: Reg, value: Reg, descriptor: u16) -> Self {
        Self::a_type(Opcode::EnumIs, AddrMode::Imm, dst, value, descriptor)
    }

    pub fn enum_field(dst: Reg, value: Reg, field: u16) -> Self {
        Self::a_type(Opcode::EnumField, AddrMode::Imm, dst, value, field)
    }

    pub fn match_fail() -> Self {
        Self::e_type(Opcode::MatchFail, 0)
    }

    pub fn load_field(dst: Reg, obj: Reg, field_idx: u32) -> Self {
        debug_assert!(field_idx < (1 << 12));
        Self::a_type(Opcode::LoadField, AddrMode::Imm, dst, obj, field_idx as u16)
    }

    pub fn store_field(obj: Reg, field_idx: u32, val: Reg) -> Self {
        debug_assert!(field_idx < (1 << 12));
        Self::a_type(
            Opcode::StoreField,
            AddrMode::Imm,
            val,
            obj,
            field_idx as u16,
        )
    }

    /// NewClosure: capture_count in base, func_id in imm12.
    pub fn new_closure(dst: Reg, func_id: u32, capture_count: u8) -> Self {
        debug_assert!(func_id < (1 << 12));
        Self::a_type(
            Opcode::NewClosure,
            AddrMode::Imm,
            dst,
            Reg(capture_count),
            func_id as u16,
        )
    }

    pub fn new_closure_wide(dst: Reg, func_id_const_idx: u16, capture_count: u8) -> Self {
        Self::a_type(
            Opcode::NewClosureWide,
            AddrMode::Imm,
            dst,
            Reg(capture_count),
            func_id_const_idx,
        )
    }

    pub fn load_capture(dst: Reg, index: u32) -> Self {
        debug_assert!(index < (1 << 12));
        Self::a_type(
            Opcode::LoadCapture,
            AddrMode::Imm,
            dst,
            Reg(0),
            index as u16,
        )
    }

    pub fn safepoint() -> Self {
        Self::e_type(Opcode::Safepoint, 0)
    }

    /// Enter a delimiter. Prompt is read from a register, arguments from r0..rN.
    /// Payload: prompt register (5), argument count (5), body FuncId (12).
    pub fn reset(prompt: Reg, body: u16, arg_count: u8) -> Self {
        assert!(prompt.0 < 32 && body < (1 << 12) && arg_count < 32);
        Self::e_type(
            Opcode::Reset,
            ((prompt.0 as u32) << 17) | ((arg_count as u32) << 12) | body as u32,
        )
    }

    /// Capture to a prompt; the resumed effect result is written to `destination`.
    /// The capture returns its internal continuation handle in the parent's r0.
    pub fn shift(prompt: Reg, destination: Reg) -> Self {
        assert!(prompt.0 < 32 && destination.0 < 32);
        Self::e_type(
            Opcode::Shift,
            ((prompt.0 as u32) << 17) | ((destination.0 as u32) << 12),
        )
    }

    /// Consume a suspended stack handle and supply the effect result.
    pub fn resume(continuation: Reg, value: Reg) -> Self {
        assert!(continuation.0 < 32 && value.0 < 32);
        Self::e_type(
            Opcode::Resume,
            ((continuation.0 as u32) << 17) | ((value.0 as u32) << 12),
        )
    }

    pub fn e_control_regs(payload: u32) -> (Reg, Reg) {
        (
            Reg(((payload >> 17) & 0x1F) as u8),
            Reg(((payload >> 12) & 0x1F) as u8),
        )
    }

    /// Dynamic invocation: argument count (5), effect TypeIndex (17).
    pub fn effect_call(effect: TypeIndex, arg_count: u8) -> Self {
        assert!(effect.as_u32() < (1 << 17) && arg_count < 32);
        Self::e_type(
            Opcode::EffectCallDyn,
            ((arg_count as u32) << 17) | effect.as_u32(),
        )
    }

    /// Install a closure handler: closure register (5), effect TypeIndex (17).
    pub fn push_handler_closure(effect: TypeIndex, closure: Reg) -> Self {
        assert!(effect.as_u32() < (1 << 17) && closure.0 < 32);
        Self::e_type(
            Opcode::PushHandlerClosure,
            ((closure.0 as u32) << 17) | effect.as_u32(),
        )
    }

    pub fn push_capturing_handler(closure: Reg, metadata: u32) -> Self {
        assert!(closure.0 < 32 && metadata < (1 << 17));
        Self::e_type(
            Opcode::PushCapturingHandler,
            ((closure.0 as u32) << 17) | metadata,
        )
    }

    pub fn reset_closure(body: Reg, handler_count: u8) -> Self {
        assert!(body.0 < 32 && handler_count < 32);
        Self::e_type(
            Opcode::ResetClosure,
            ((body.0 as u32) << 17) | ((handler_count as u32) << 12),
        )
    }

    pub fn resume_continuation(continuation: Reg, value: Reg) -> Self {
        assert!(continuation.0 < 32 && value.0 < 32);
        Self::e_type(
            Opcode::ResumeContinuation,
            ((continuation.0 as u32) << 17) | ((value.0 as u32) << 12),
        )
    }

    pub fn clone_continuation(destination: Reg, source: Reg) -> Self {
        assert!(destination.0 < 32 && source.0 < 32);
        Self::e_type(
            Opcode::CloneContinuation,
            ((destination.0 as u32) << 17) | ((source.0 as u32) << 12),
        )
    }

    /// Consume a source continuation proven to have no other observable uses.
    pub fn resume_continuation_once(continuation: Reg, value: Reg) -> Self {
        assert!(continuation.0 < 32 && value.0 < 32);
        Self::e_type(
            Opcode::ResumeContinuationOnce,
            ((continuation.0 as u32) << 17) | ((value.0 as u32) << 12),
        )
    }

    pub fn drop_continuation(source: Reg) -> Self {
        assert!(source.0 < 32);
        Self::e_type(Opcode::DropContinuation, (source.0 as u32) << 17)
    }

    pub fn nop() -> Self {
        Self::e_type(Opcode::Nop, 0)
    }

    // -- C-type payload helpers --

    pub fn c_arg_count_func_id(payload: u32) -> (u8, u32) {
        let arg_count = ((payload >> 14) & 0xFF) as u8;
        let func_id = payload & 0x3FFF;
        (arg_count, func_id)
    }

    pub fn c_call_indirect(payload: u32) -> (u8, Reg) {
        let arg_count = ((payload >> 14) & 0xFF) as u8;
        let closure = Reg(((payload >> 9) & 0x1F) as u8);
        (arg_count, closure)
    }

    pub fn c_call_method(payload: u32) -> (u8, Reg, u32) {
        let arg_count = ((payload >> 17) & 0x1F) as u8;
        let recv = Reg(((payload >> 12) & 0x1F) as u8);
        let method_id = payload & IMM12_MASK;
        (arg_count, recv, method_id)
    }

    pub fn c_return_reg(payload: u32) -> Reg {
        Reg(((payload >> 17) & 0x1F) as u8)
    }

    /// Sign-extend the 12-bit imm field to i32 (for RegOff / signed Imm).
    pub fn sext_imm12(imm12: u16) -> i32 {
        sign_extend(imm12 as u32, 12)
    }

    /// Decode LoadConstWide 19-bit index.
    pub fn wide_const_index(base: Reg, imm12: u16) -> u32 {
        ((base.0 as u32) << 12) | (imm12 as u32 & IMM12_MASK)
    }
}

// ---------------------------------------------------------------------------
// FuncHeader — metadata for a function's bytecode block
// ---------------------------------------------------------------------------

/// Metadata header for a compiled function in the code section.
#[derive(Debug, Clone)]
pub struct FuncHeader {
    pub func_id: FuncId,
    pub register_count: u8,
    pub param_count: u8,
    pub has_variadic: bool,
    pub is_closure: bool,
}

// ---------------------------------------------------------------------------
// ConstantPool entry
// ---------------------------------------------------------------------------

/// A value stored in the constant pool.
#[derive(Debug, Clone)]
pub enum Constant {
    Int(i64),
    UInt(u64),
    Int128(i128),
    UInt128(u128),
    /// A reference to an entry in the artifact's type pool, not a stable TypeId.
    Type(TypeIndex),
    /// Enum identity/tag descriptor; only nullary variants can be loaded directly.
    Enum {
        type_index: TypeIndex,
        variant: u32,
    },
    Float(f64),
    Str(String),
    /// A Unicode scalar value, loaded as a character immediate.
    Char(char),
    BigInt(Vec<u8>),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[test]
    fn trait_operations_preserve_views_slots_and_physical_counts() {
        let view = type_pool::TypeIndex::from_raw(4095);
        for instruction in [
            super::Instruction::trait_proof(super::Reg(31), super::Reg(30), view),
            super::Instruction::trait_assert(super::Reg(1), super::Reg(2), view),
            super::Instruction::trait_project(super::Reg(3), super::Reg(4), view),
            super::Instruction::trait_call(super::Reg(31), 4095, 31),
            super::Instruction::call_indirect_proof(super::Reg(31), 31),
        ] {
            let decoded = super::Instruction::decode(instruction.encode()).unwrap();
            assert_eq!(decoded, instruction);
        }
        let call = super::Instruction::trait_call(super::Reg(31), 4095, 31);
        let super::InstructionData::C { payload } = call.data else {
            panic!("call encoding");
        };
        assert_eq!(
            super::Instruction::c_call_method(payload),
            (31, super::Reg(31), 4095)
        );
    }

    #[test]
    fn enum_and_match_failure_opcodes_preserve_their_operands() {
        for instruction in [
            super::Instruction::new_enum(super::Reg(31), super::Reg(30), 4095),
            super::Instruction::enum_is(super::Reg(3), super::Reg(2), 123),
            super::Instruction::enum_field(super::Reg(2), super::Reg(1), 4095),
            super::Instruction::match_fail(),
            super::Instruction::a_type(
                super::Opcode::ErrorOk,
                super::AddrMode::Imm,
                super::Reg(31),
                super::Reg(30),
                4095,
            ),
            super::Instruction::a_type(
                super::Opcode::ErrorErr,
                super::AddrMode::Imm,
                super::Reg(31),
                super::Reg(30),
                4095,
            ),
            super::Instruction::a_type(
                super::Opcode::ErrorIsOk,
                super::AddrMode::Imm,
                super::Reg(31),
                super::Reg(30),
                0,
            ),
            super::Instruction::a_type(
                super::Opcode::ErrorPayload,
                super::AddrMode::Imm,
                super::Reg(31),
                super::Reg(30),
                0,
            ),
        ] {
            let decoded = super::Instruction::decode(instruction.encode()).unwrap();
            assert_eq!(decoded.opcode, instruction.opcode);
            assert_eq!(decoded.encode(), instruction.encode());
        }
        assert_eq!(super::Opcode::from_u8(0x71), None);
        assert_eq!(super::Opcode::from_u8(0xd5), None);
    }

    use super::*;

    #[test]
    fn encode_decode_r_type() {
        let instr = Instruction::add(Reg(0), Reg(1), Reg(2));
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
    }

    #[test]
    fn encode_decode_load_imm() {
        let instr = Instruction::load_imm(Reg(3), 0xABC);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
        assert_eq!(decoded.amode, AddrMode::Imm);
    }

    #[test]
    fn encode_decode_load_const() {
        let instr = Instruction::load_const(Reg(1), 42);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
        assert_eq!(decoded.amode, AddrMode::Const);
    }

    #[test]
    fn encode_decode_type_assert_preserves_boundary_operands() {
        let instruction =
            Instruction::a_type(Opcode::TypeAssert, AddrMode::Imm, Reg(31), Reg(30), 4095);
        assert_eq!(Instruction::decode(instruction.encode()), Some(instruction));
    }

    #[test]
    fn encode_decode_load_reg_off() {
        let instr = Instruction::load_reg_off(Reg(2), Reg(5), -3);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded.opcode, Opcode::Load);
        assert_eq!(decoded.amode, AddrMode::RegOff);
        if let InstructionData::A { dst, base, imm12 } = decoded.data {
            assert_eq!(dst.0, 2);
            assert_eq!(base.0, 5);
            assert_eq!(Instruction::sext_imm12(imm12), -3);
        } else {
            panic!("expected A-type");
        }
    }

    #[test]
    fn reserved_amode_rejected_for_load() {
        let word = (Opcode::Load as u32) << 24 | (0b11u32 << 22) | ((3u32) << 17);
        assert!(Instruction::decode(word).is_none());
    }

    #[test]
    fn encode_decode_j_type_positive() {
        let instr = Instruction::jmp(42);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
    }

    #[test]
    fn encode_decode_j_type_negative() {
        let instr = Instruction::jmp(-10);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
    }

    #[test]
    fn encode_decode_call() {
        let instr = Instruction::call(7, 3);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded.opcode, Opcode::Call);
        if let InstructionData::C { payload } = decoded.data {
            let (argc, fid) = Instruction::c_arg_count_func_id(payload);
            assert_eq!(argc, 3);
            assert_eq!(fid, 7);
        } else {
            panic!("expected C-type");
        }
    }

    #[test]
    fn encode_decode_e_type() {
        let instr = Instruction::safepoint();
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
    }

    #[test]
    fn source_effect_instructions_preserve_their_payload_fields() {
        let effect = TypeIndex::from_raw(0x1FFFF);
        let fixtures = [
            (
                Instruction::effect_call(effect, 31),
                Opcode::EffectCallDyn,
                (31 << 17) | 0x1FFFF,
            ),
            (
                Instruction::push_handler_closure(effect, Reg(31)),
                Opcode::PushHandlerClosure,
                (31 << 17) | 0x1FFFF,
            ),
            (
                Instruction::push_capturing_handler(Reg(30), 0x1FFFF),
                Opcode::PushCapturingHandler,
                (30 << 17) | 0x1FFFF,
            ),
            (
                Instruction::reset_closure(Reg(29), 31),
                Opcode::ResetClosure,
                (29 << 17) | (31 << 12),
            ),
            (
                Instruction::resume_continuation(Reg(28), Reg(27)),
                Opcode::ResumeContinuation,
                (28 << 17) | (27 << 12),
            ),
            (
                Instruction::resume_continuation_once(Reg(26), Reg(25)),
                Opcode::ResumeContinuationOnce,
                (26 << 17) | (25 << 12),
            ),
        ];
        for (instruction, opcode, payload) in fixtures {
            assert_eq!(instruction.encode(), ((opcode as u32) << 24) | payload);
            assert_eq!(Instruction::decode(instruction.encode()), Some(instruction));
        }
    }

    #[test]
    fn opcode_format_families() {
        assert_eq!(Opcode::Add.format(), Format::R);
        assert_eq!(Opcode::Load.format(), Format::A);
        assert_eq!(Opcode::Jmp.format(), Format::J);
        assert_eq!(Opcode::Call.format(), Format::C);
        assert_eq!(Opcode::Safepoint.format(), Format::E);
    }

    #[test]
    fn local_slot_instructions_preserve_wide_indices() {
        for index in [0, 4095, 4096, 70000, (1 << 17) - 1] {
            for instruction in [
                Instruction::load_slot(Reg(17), index),
                Instruction::store_slot(index, Reg(16)),
            ] {
                let decoded = Instruction::decode(instruction.encode()).unwrap();
                assert_eq!(decoded, instruction);
                let InstructionData::A { base, imm12, .. } = decoded.data else {
                    panic!("expected slot instruction");
                };
                assert_eq!(((base.0 as u32) << 12) | imm12 as u32, index);
            }
        }
        let allocation = Instruction::allocate_slots(1 << 17);
        assert_eq!(Instruction::decode(allocation.encode()), Some(allocation));
    }

    #[test]
    fn mov_roundtrip() {
        let instr = Instruction::mov(Reg(5), Reg(10));
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded.opcode, Opcode::Mov);
        if let InstructionData::R { dst, src1, .. } = decoded.data {
            assert_eq!(dst.0, 5);
            assert_eq!(src1.0, 10);
        } else {
            panic!("expected R-type");
        }
    }

    #[test]
    fn far_jumps_preserve_all_twenty_two_signed_offset_bits() {
        for offset in [-(1 << 21), -70000, -1, 0, 70000, (1 << 21) - 1] {
            let instruction = Instruction::jmp_far(offset);
            let word = instruction.encode();
            assert_eq!(word & PAYLOAD22_MASK, offset as u32 & PAYLOAD22_MASK);
            assert_eq!(Instruction::decode(word), Some(instruction));
        }
    }

    #[test]
    fn load_const_wide_roundtrip() {
        let idx = (3u32 << 12) | 0xABC;
        let instr = Instruction::load_const_wide(Reg(4), idx);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        if let InstructionData::A { dst, base, imm12 } = decoded.data {
            assert_eq!(dst.0, 4);
            assert_eq!(Instruction::wide_const_index(base, imm12), idx);
        } else {
            panic!("expected A-type");
        }
    }
}
