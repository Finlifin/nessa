//! NSBC instruction set definitions.
//!
//! Fixed 64-bit instruction encoding with 4 format families:
//! R-type (register-register), I-type (register-immediate),
//! J-type (jump/call), E-type (effect/system).

use type_pool::TypeIndex;

// ---------------------------------------------------------------------------
// IntrinsicFn — native functions known to the VM
// ---------------------------------------------------------------------------

/// Intrinsic functions implemented natively by the VM.
///
/// These are the primitive operations that cannot be implemented in nessa
/// source code.  User-facing names are defined in the `std` package via the
/// `'intrinsic` mechanism; the compiler emits [`Opcode::CallIntrinsic`]
/// instructions referencing these variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum IntrinsicFn {
    // ── I/O ────────────────────────────────────────────────────────
    /// Print a value to stdout (no trailing newline).
    Print = 0,
    /// Print a value to stdout followed by a newline.
    PrintLn = 1,

    // ── Type introspection ─────────────────────────────────────────
    /// Get the runtime type of a value.
    TypeOf = 2,

    // ── Numeric conversions ────────────────────────────────────────
    /// Convert to i64.
    ToI64 = 3,
    /// Convert to f64.
    ToF64 = 4,
    /// Convert to string.
    ToString = 5,

    // ── Math ───────────────────────────────────────────────────────
    Abs = 10,
    Sin = 11,
    Cos = 12,
    Sqrt = 13,
    Floor = 14,
    Ceil = 15,
    Round = 16,
    Pow = 17,
    Log = 18,

    // ── String ─────────────────────────────────────────────────────
    StrLen = 30,
    StrConcat = 31,

    // ── Process ────────────────────────────────────────────────────
    /// Terminate the process.
    Exit = 50,
    /// Panic with a message.
    Panic = 51,
}

impl IntrinsicFn {
    /// Create from a raw u16.  Returns `None` for unknown values.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0 => Some(Self::Print),
            1 => Some(Self::PrintLn),
            2 => Some(Self::TypeOf),
            3 => Some(Self::ToI64),
            4 => Some(Self::ToF64),
            5 => Some(Self::ToString),
            10 => Some(Self::Abs),
            11 => Some(Self::Sin),
            12 => Some(Self::Cos),
            13 => Some(Self::Sqrt),
            14 => Some(Self::Floor),
            15 => Some(Self::Ceil),
            16 => Some(Self::Round),
            17 => Some(Self::Pow),
            18 => Some(Self::Log),
            30 => Some(Self::StrLen),
            31 => Some(Self::StrConcat),
            50 => Some(Self::Exit),
            51 => Some(Self::Panic),
            _ => None,
        }
    }

    /// The user-facing name of this intrinsic (as used in `std`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::PrintLn => "println",
            Self::TypeOf => "type_of",
            Self::ToI64 => "to_i64",
            Self::ToF64 => "to_f64",
            Self::ToString => "to_string",
            Self::Abs => "abs",
            Self::Sin => "sin",
            Self::Cos => "cos",
            Self::Sqrt => "sqrt",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Round => "round",
            Self::Pow => "pow",
            Self::Log => "log",
            Self::StrLen => "str_len",
            Self::StrConcat => "str_concat",
            Self::Exit => "exit",
            Self::Panic => "panic",
        }
    }

    /// All defined intrinsic functions.
    pub const ALL: &'static [IntrinsicFn] = &[
        Self::Print, Self::PrintLn,
        Self::TypeOf,
        Self::ToI64, Self::ToF64, Self::ToString,
        Self::Abs, Self::Sin, Self::Cos, Self::Sqrt,
        Self::Floor, Self::Ceil, Self::Round, Self::Pow, Self::Log,
        Self::StrLen, Self::StrConcat,
        Self::Exit, Self::Panic,
    ];
}

// ---------------------------------------------------------------------------
// Register and FuncId newtypes
// ---------------------------------------------------------------------------

/// A register number (0..19 for GP, rest reserved).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u8);

impl Reg {
    pub const MAX_GP: u8 = 20;
}

/// Identifies a function in the bytecode store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

// ---------------------------------------------------------------------------
// Opcode — all bytecode operations
// ---------------------------------------------------------------------------

/// All opcodes for the NSBC instruction set.
/// High 2 bits encode the format family (R=00, I=01, J=10, E=11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Opcode {
    // ── R-type (00): arithmetic & register ops ─────────────────
    Add        = 0x00,
    Sub        = 0x01,
    Mul        = 0x02,
    Div        = 0x03,
    Mod        = 0x04,
    Neg        = 0x05,
    BitAnd     = 0x06,
    BitOr      = 0x07,
    BitXor     = 0x08,
    BitNot     = 0x09,
    Shl        = 0x0A,
    Shr        = 0x0B,
    UShr       = 0x0C,

    // ── R-type (00): comparison ────────────────────────────────
    CmpEq      = 0x10,
    CmpNe      = 0x11,
    CmpLt      = 0x12,
    CmpLe      = 0x13,
    CmpGt      = 0x14,
    CmpGe      = 0x15,

    // ── R-type (00): register manipulation ─────────────────────
    Mov        = 0x18,
    Swap       = 0x19,

    // ── I-type (01): constants & types ─────────────────────────
    LoadImm       = 0x40,
    LoadConst     = 0x41,
    LoadUnit      = 0x42,
    LoadTrue      = 0x43,
    LoadFalse     = 0x44,
    LoadNull      = 0x45,
    TypeCheck     = 0x48,
    TypeCast      = 0x49,
    TypeCastSafe  = 0x4A,

    // ── I-type (01): memory access ─────────────────────────────
    LoadField     = 0x50,
    StoreField    = 0x51,
    LoadIndex     = 0x52,
    StoreIndex    = 0x53,
    LoadGlobal    = 0x54,
    StoreGlobal   = 0x55,
    LoadCapture   = 0x56,

    // ── I-type (01): object creation ───────────────────────────
    NewObject     = 0x60,
    NewList       = 0x61,
    NewMap        = 0x62,
    NewClosure    = 0x63,

    // ── J-type (10): control flow ──────────────────────────────
    Jmp           = 0x80,
    JmpIf         = 0x81,
    JmpIfNot      = 0x82,
    JmpIfNull     = 0x83,
    JmpIfNotNull  = 0x84,

    // ── J-type (10): calls ─────────────────────────────────────
    Call          = 0x88,
    CallIndirect  = 0x89,
    CallMethod    = 0x8A,
    CallWasm      = 0x8B,
    TailCall      = 0x8C,
    CallIntrinsic = 0x8D,
    ReturnUnit    = 0x8E,
    Return        = 0x8F,

    // ── E-type (11): effects & continuations ───────────────────
    EffectCall    = 0xC0,
    EffectCallDyn = 0xC1,
    PushHandler   = 0xC2,
    PopHandler    = 0xC3,
    Shift         = 0xC4,
    Reset         = 0xC5,
    Resume        = 0xC6,

    // ── E-type (11): system ────────────────────────────────────
    Safepoint     = 0xD0,
    DebugBreak    = 0xD1,
    Nop           = 0xD2,
}

impl Opcode {
    pub const fn format(self) -> Format {
        match (self as u8) >> 6 {
            0b00 => Format::R,
            0b01 => Format::I,
            0b10 => Format::J,
            _    => Format::E,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        // Only accept known opcodes
        match v {
            0x00..=0x0C => Some(unsafe { std::mem::transmute(v) }),
            0x10..=0x15 => Some(unsafe { std::mem::transmute(v) }),
            0x18..=0x19 => Some(unsafe { std::mem::transmute(v) }),
            0x40..=0x45 => Some(unsafe { std::mem::transmute(v) }),
            0x48..=0x4A => Some(unsafe { std::mem::transmute(v) }),
            0x50..=0x56 => Some(unsafe { std::mem::transmute(v) }),
            0x60..=0x63 => Some(unsafe { std::mem::transmute(v) }),
            0x80..=0x84 => Some(unsafe { std::mem::transmute(v) }),
            0x88..=0x8D | 0x8E..=0x8F => Some(unsafe { std::mem::transmute(v) }),
            0xC0..=0xC6 => Some(unsafe { std::mem::transmute(v) }),
            0xD0..=0xD2 => Some(unsafe { std::mem::transmute(v) }),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Instruction format
// ---------------------------------------------------------------------------

/// Instruction format family, derived from opcode bits [7:6].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// R-type: dst(5) src1(5) src2(5) aux(41)
    R,
    /// I-type: dst(5) src(5) imm(46)
    I,
    /// J-type: cond(5) offset(51)
    J,
    /// E-type: payload(56)
    E,
}

// ---------------------------------------------------------------------------
// Instruction — a decoded 64-bit instruction
// ---------------------------------------------------------------------------

/// A single decoded NSBC instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    pub opcode: Opcode,
    pub data: InstructionData,
}

/// The decoded operands, varying by format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionData {
    R {
        dst: Reg,
        src1: Reg,
        src2: Reg,
        aux: u64, // 41 bits
    },
    I {
        dst: Reg,
        src: Reg,
        imm: u64, // 46 bits
    },
    J {
        cond: Reg,
        offset: i64, // 51 bits sign-extended
    },
    E {
        payload: u64, // 56 bits
    },
}

// ---------------------------------------------------------------------------
// Encoding / decoding
// ---------------------------------------------------------------------------

impl Instruction {
    /// Encode to a 64-bit word.
    pub fn encode(self) -> u64 {
        let op = self.opcode as u64;
        match self.data {
            InstructionData::R { dst, src1, src2, aux } => {
                (op << 56)
                    | ((dst.0 as u64 & 0x1F) << 51)
                    | ((src1.0 as u64 & 0x1F) << 46)
                    | ((src2.0 as u64 & 0x1F) << 41)
                    | (aux & 0x1FF_FFFF_FFFF) // 41 bits
            }
            InstructionData::I { dst, src, imm } => {
                (op << 56)
                    | ((dst.0 as u64 & 0x1F) << 51)
                    | ((src.0 as u64 & 0x1F) << 46)
                    | (imm & 0x3FFF_FFFF_FFFF) // 46 bits
            }
            InstructionData::J { cond, offset } => {
                let off_bits = (offset as u64) & 0x7_FFFF_FFFF_FFFF; // 51 bits
                (op << 56)
                    | ((cond.0 as u64 & 0x1F) << 51)
                    | off_bits
            }
            InstructionData::E { payload } => {
                (op << 56) | (payload & 0x00FF_FFFF_FFFF_FFFF) // 56 bits
            }
        }
    }

    /// Decode from a 64-bit word.  Returns `None` if the opcode is unknown.
    pub fn decode(word: u64) -> Option<Self> {
        let op_byte = (word >> 56) as u8;
        let opcode = Opcode::from_u8(op_byte)?;
        let data = match opcode.format() {
            Format::R => InstructionData::R {
                dst: Reg(((word >> 51) & 0x1F) as u8),
                src1: Reg(((word >> 46) & 0x1F) as u8),
                src2: Reg(((word >> 41) & 0x1F) as u8),
                aux: word & 0x1FF_FFFF_FFFF,
            },
            Format::I => InstructionData::I {
                dst: Reg(((word >> 51) & 0x1F) as u8),
                src: Reg(((word >> 46) & 0x1F) as u8),
                imm: word & 0x3FFF_FFFF_FFFF,
            },
            Format::J => {
                let cond = Reg(((word >> 51) & 0x1F) as u8);
                let raw_offset = word & 0x7_FFFF_FFFF_FFFF; // 51 bits
                // sign-extend from 51 bits
                let offset = if raw_offset & (1 << 50) != 0 {
                    (raw_offset | 0xFFFF_E000_0000_0000) as i64
                } else {
                    raw_offset as i64
                };
                InstructionData::J { cond, offset }
            }
            Format::E => InstructionData::E {
                payload: word & 0x00FF_FFFF_FFFF_FFFF,
            },
        };
        Some(Instruction { opcode, data })
    }
}

// ---------------------------------------------------------------------------
// Convenient constructors
// ---------------------------------------------------------------------------

impl Instruction {
    pub fn r_type(opcode: Opcode, dst: Reg, src1: Reg, src2: Reg) -> Self {
        Self {
            opcode,
            data: InstructionData::R { dst, src1, src2, aux: 0 },
        }
    }

    pub fn i_type(opcode: Opcode, dst: Reg, src: Reg, imm: u64) -> Self {
        Self {
            opcode,
            data: InstructionData::I { dst, src, imm },
        }
    }

    pub fn j_type(opcode: Opcode, cond: Reg, offset: i64) -> Self {
        Self {
            opcode,
            data: InstructionData::J { cond, offset },
        }
    }

    pub fn e_type(opcode: Opcode, payload: u64) -> Self {
        Self {
            opcode,
            data: InstructionData::E { payload },
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

    pub fn load_imm(dst: Reg, imm: u64) -> Self {
        Self::i_type(Opcode::LoadImm, dst, Reg(0), imm)
    }

    pub fn load_const(dst: Reg, const_idx: u32) -> Self {
        Self::i_type(Opcode::LoadConst, dst, Reg(0), const_idx as u64)
    }

    pub fn load_unit(dst: Reg) -> Self {
        Self::i_type(Opcode::LoadUnit, dst, Reg(0), 0)
    }

    pub fn load_true(dst: Reg) -> Self {
        Self::i_type(Opcode::LoadTrue, dst, Reg(0), 0)
    }

    pub fn load_false(dst: Reg) -> Self {
        Self::i_type(Opcode::LoadFalse, dst, Reg(0), 0)
    }

    pub fn load_null(dst: Reg) -> Self {
        Self::i_type(Opcode::LoadNull, dst, Reg(0), 0)
    }

    pub fn jmp(offset: i64) -> Self {
        Self::j_type(Opcode::Jmp, Reg(0), offset)
    }

    pub fn jmp_if(cond: Reg, offset: i64) -> Self {
        Self::j_type(Opcode::JmpIf, cond, offset)
    }

    pub fn jmp_if_not(cond: Reg, offset: i64) -> Self {
        Self::j_type(Opcode::JmpIfNot, cond, offset)
    }

    pub fn call(func_id: u32, arg_count: u8) -> Self {
        let payload = ((func_id as u64) << 8) | (arg_count as u64);
        Self::j_type(Opcode::Call, Reg(0), payload as i64)
    }

    pub fn call_intrinsic(intrinsic: IntrinsicFn, arg_count: u8) -> Self {
        let payload = ((intrinsic as u64) << 8) | (arg_count as u64);
        Self::j_type(Opcode::CallIntrinsic, Reg(0), payload as i64)
    }

    pub fn ret(src: Reg) -> Self {
        Self::j_type(Opcode::Return, src, 0)
    }

    pub fn return_unit() -> Self {
        Self::j_type(Opcode::ReturnUnit, Reg(0), 0)
    }

    /// Create a CallIndirect instruction for calling a closure.
    /// `closure_reg` holds the closure value, `arg_count` is the number of explicit args in r0..r(N-1).
    pub fn call_indirect(closure_reg: Reg, arg_count: u8) -> Self {
        Self::j_type(Opcode::CallIndirect, closure_reg, arg_count as i64)
    }

    /// Create a CallMethod instruction for method dispatch.
    /// `receiver_reg` holds the receiver object, args are in r0..r(arg_count-1).
    /// `method_str_id` identifies the method name via the string interner.
    pub fn call_method(receiver_reg: Reg, method_str_id: u32, arg_count: u8) -> Self {
        let payload = ((method_str_id as u64) << 8) | (arg_count as u64);
        Self::j_type(Opcode::CallMethod, receiver_reg, payload as i64)
    }

    pub fn new_object(dst: Reg, type_idx: TypeIndex) -> Self {
        Self::i_type(Opcode::NewObject, dst, Reg(0), type_idx.as_u32() as u64)
    }

    pub fn load_field(dst: Reg, obj: Reg, field_idx: u32) -> Self {
        Self::i_type(Opcode::LoadField, dst, obj, field_idx as u64)
    }

    pub fn store_field(obj: Reg, field_idx: u32, val: Reg) -> Self {
        Self::i_type(Opcode::StoreField, val, obj, field_idx as u64)
    }

    /// Create a NewClosure instruction.
    /// The func_id is stored in the immediate, and captures are in r0..r(count-1).
    pub fn new_closure(dst: Reg, func_id: u32, capture_count: u8) -> Self {
        // Encode func_id in the immediate, capture_count in src1.
        Self::i_type(Opcode::NewClosure, dst, Reg(capture_count), func_id as u64)
    }

    /// Create a LoadCapture instruction.
    /// Loads capture at `index` from the current closure environment into `dst`.
    pub fn load_capture(dst: Reg, index: u32) -> Self {
        Self::i_type(Opcode::LoadCapture, dst, Reg(0), index as u64)
    }

    pub fn safepoint() -> Self {
        Self::e_type(Opcode::Safepoint, 0)
    }

    pub fn nop() -> Self {
        Self::e_type(Opcode::Nop, 0)
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
    Float(f64),
    Str(String),
    BigInt(Vec<u8>),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_r_type() {
        let instr = Instruction::add(Reg(0), Reg(1), Reg(2));
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
    }

    #[test]
    fn encode_decode_i_type() {
        let instr = Instruction::load_imm(Reg(3), 12345);
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
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
    fn encode_decode_e_type() {
        let instr = Instruction::safepoint();
        let word = instr.encode();
        let decoded = Instruction::decode(word).unwrap();
        assert_eq!(decoded, instr);
    }

    #[test]
    fn opcode_format_families() {
        assert_eq!(Opcode::Add.format(), Format::R);
        assert_eq!(Opcode::LoadImm.format(), Format::I);
        assert_eq!(Opcode::Jmp.format(), Format::J);
        assert_eq!(Opcode::Safepoint.format(), Format::E);
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
}
