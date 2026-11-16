//! 128-bit SIMD (`simd128`) instruction lowering and execution for `engine/wasm`.

use wasmparser::Operator;

use crate::module::MemImmediate;
use crate::{WasmTrap, WasmVal};

#[derive(Debug, Clone)]
pub enum SimdOp {
    V128Const(u128),
    V128Load(MemImmediate),
    V128Load8Splat(MemImmediate),
    V128Load16Splat(MemImmediate),
    V128Load32Splat(MemImmediate),
    V128Load64Splat(MemImmediate),
    V128Load32Zero(MemImmediate),
    V128Load64Zero(MemImmediate),
    V128Load8Lane(MemImmediate, u8),
    V128Load16Lane(MemImmediate, u8),
    V128Load32Lane(MemImmediate, u8),
    V128Load64Lane(MemImmediate, u8),
    V128Store(MemImmediate),
    V128Store8Lane(MemImmediate, u8),
    V128Store16Lane(MemImmediate, u8),
    V128Store32Lane(MemImmediate, u8),
    V128Store64Lane(MemImmediate, u8),

    I8x16Shuffle([u8; 16]),
    I8x16Swizzle,

    I8x16Splat,
    I16x8Splat,
    I32x4Splat,
    I64x2Splat,
    F32x4Splat,
    F64x2Splat,

    I8x16ExtractLaneS(u8),
    I8x16ExtractLaneU(u8),
    I8x16ReplaceLane(u8),
    I16x8ExtractLaneS(u8),
    I16x8ExtractLaneU(u8),
    I16x8ReplaceLane(u8),
    I32x4ExtractLane(u8),
    I32x4ReplaceLane(u8),
    I64x2ExtractLane(u8),
    I64x2ReplaceLane(u8),
    F32x4ExtractLane(u8),
    F32x4ReplaceLane(u8),
    F64x2ExtractLane(u8),
    F64x2ReplaceLane(u8),

    V128Not,
    V128And,
    V128AndNot,
    V128Or,
    V128Xor,
    V128Bitselect,
    V128AnyTrue,

    I8x16AllTrue,
    I8x16Bitmask,
    I8x16Eq,
    I8x16Ne,
    I8x16LtS,
    I8x16LtU,
    I8x16GtS,
    I8x16GtU,
    I8x16LeS,
    I8x16LeU,
    I8x16GeS,
    I8x16GeU,
    I8x16Neg,
    I8x16Abs,
    I8x16Add,
    I8x16AddSatS,
    I8x16AddSatU,
    I8x16Sub,
    I8x16SubSatS,
    I8x16SubSatU,
    I8x16Shl,
    I8x16ShrS,
    I8x16ShrU,

    I16x8AllTrue,
    I16x8Bitmask,
    I16x8Eq,
    I16x8Ne,
    I16x8Neg,
    I16x8Abs,
    I16x8Add,
    I16x8Sub,
    I16x8Mul,
    I16x8Shl,
    I16x8ShrS,
    I16x8ShrU,

    I32x4AllTrue,
    I32x4Bitmask,
    I32x4Eq,
    I32x4Ne,
    I32x4LtS,
    I32x4LtU,
    I32x4GtS,
    I32x4GtU,
    I32x4LeS,
    I32x4LeU,
    I32x4GeS,
    I32x4GeU,
    I32x4Neg,
    I32x4Abs,
    I32x4Add,
    I32x4Sub,
    I32x4Mul,
    I32x4Shl,
    I32x4ShrS,
    I32x4ShrU,

    I64x2AllTrue,
    I64x2Bitmask,
    I64x2Eq,
    I64x2Ne,
    I64x2Neg,
    I64x2Abs,
    I64x2Add,
    I64x2Sub,
    I64x2Mul,
    I64x2Shl,
    I64x2ShrS,
    I64x2ShrU,

    F32x4Eq,
    F32x4Ne,
    F32x4Lt,
    F32x4Gt,
    F32x4Le,
    F32x4Ge,
    F32x4Abs,
    F32x4Neg,
    F32x4Sqrt,
    F32x4Add,
    F32x4Sub,
    F32x4Mul,
    F32x4Div,
    F32x4Min,
    F32x4Max,

    F64x2Eq,
    F64x2Ne,
    F64x2Lt,
    F64x2Gt,
    F64x2Le,
    F64x2Ge,
    F64x2Abs,
    F64x2Neg,
    F64x2Sqrt,
    F64x2Add,
    F64x2Sub,
    F64x2Mul,
    F64x2Div,
    F64x2Min,
    F64x2Max,
}

pub(crate) fn lower_simd_operator(op: &Operator<'_>) -> Option<SimdOp> {
    Some(match *op {
        Operator::V128Const { value } => SimdOp::V128Const(u128::from_le_bytes(*value.bytes())),
        Operator::V128Load { memarg } => SimdOp::V128Load(memarg.into()),
        Operator::V128Load8Splat { memarg } => SimdOp::V128Load8Splat(memarg.into()),
        Operator::V128Load16Splat { memarg } => SimdOp::V128Load16Splat(memarg.into()),
        Operator::V128Load32Splat { memarg } => SimdOp::V128Load32Splat(memarg.into()),
        Operator::V128Load64Splat { memarg } => SimdOp::V128Load64Splat(memarg.into()),
        Operator::V128Load32Zero { memarg } => SimdOp::V128Load32Zero(memarg.into()),
        Operator::V128Load64Zero { memarg } => SimdOp::V128Load64Zero(memarg.into()),
        Operator::V128Load8Lane { memarg, lane } => SimdOp::V128Load8Lane(memarg.into(), lane),
        Operator::V128Load16Lane { memarg, lane } => SimdOp::V128Load16Lane(memarg.into(), lane),
        Operator::V128Load32Lane { memarg, lane } => SimdOp::V128Load32Lane(memarg.into(), lane),
        Operator::V128Load64Lane { memarg, lane } => SimdOp::V128Load64Lane(memarg.into(), lane),
        Operator::V128Store { memarg } => SimdOp::V128Store(memarg.into()),
        Operator::V128Store8Lane { memarg, lane } => SimdOp::V128Store8Lane(memarg.into(), lane),
        Operator::V128Store16Lane { memarg, lane } => SimdOp::V128Store16Lane(memarg.into(), lane),
        Operator::V128Store32Lane { memarg, lane } => SimdOp::V128Store32Lane(memarg.into(), lane),
        Operator::V128Store64Lane { memarg, lane } => SimdOp::V128Store64Lane(memarg.into(), lane),

        Operator::I8x16Shuffle { lanes } => SimdOp::I8x16Shuffle(lanes),
        Operator::I8x16Swizzle => SimdOp::I8x16Swizzle,

        Operator::I8x16Splat => SimdOp::I8x16Splat,
        Operator::I16x8Splat => SimdOp::I16x8Splat,
        Operator::I32x4Splat => SimdOp::I32x4Splat,
        Operator::I64x2Splat => SimdOp::I64x2Splat,
        Operator::F32x4Splat => SimdOp::F32x4Splat,
        Operator::F64x2Splat => SimdOp::F64x2Splat,

        Operator::I8x16ExtractLaneS { lane } => SimdOp::I8x16ExtractLaneS(lane),
        Operator::I8x16ExtractLaneU { lane } => SimdOp::I8x16ExtractLaneU(lane),
        Operator::I8x16ReplaceLane { lane } => SimdOp::I8x16ReplaceLane(lane),
        Operator::I16x8ExtractLaneS { lane } => SimdOp::I16x8ExtractLaneS(lane),
        Operator::I16x8ExtractLaneU { lane } => SimdOp::I16x8ExtractLaneU(lane),
        Operator::I16x8ReplaceLane { lane } => SimdOp::I16x8ReplaceLane(lane),
        Operator::I32x4ExtractLane { lane } => SimdOp::I32x4ExtractLane(lane),
        Operator::I32x4ReplaceLane { lane } => SimdOp::I32x4ReplaceLane(lane),
        Operator::I64x2ExtractLane { lane } => SimdOp::I64x2ExtractLane(lane),
        Operator::I64x2ReplaceLane { lane } => SimdOp::I64x2ReplaceLane(lane),
        Operator::F32x4ExtractLane { lane } => SimdOp::F32x4ExtractLane(lane),
        Operator::F32x4ReplaceLane { lane } => SimdOp::F32x4ReplaceLane(lane),
        Operator::F64x2ExtractLane { lane } => SimdOp::F64x2ExtractLane(lane),
        Operator::F64x2ReplaceLane { lane } => SimdOp::F64x2ReplaceLane(lane),

        Operator::V128Not => SimdOp::V128Not,
        Operator::V128And => SimdOp::V128And,
        Operator::V128AndNot => SimdOp::V128AndNot,
        Operator::V128Or => SimdOp::V128Or,
        Operator::V128Xor => SimdOp::V128Xor,
        Operator::V128Bitselect => SimdOp::V128Bitselect,
        Operator::V128AnyTrue => SimdOp::V128AnyTrue,

        Operator::I8x16AllTrue => SimdOp::I8x16AllTrue,
        Operator::I8x16Bitmask => SimdOp::I8x16Bitmask,
        Operator::I8x16Eq => SimdOp::I8x16Eq,
        Operator::I8x16Ne => SimdOp::I8x16Ne,
        Operator::I8x16LtS => SimdOp::I8x16LtS,
        Operator::I8x16LtU => SimdOp::I8x16LtU,
        Operator::I8x16GtS => SimdOp::I8x16GtS,
        Operator::I8x16GtU => SimdOp::I8x16GtU,
        Operator::I8x16LeS => SimdOp::I8x16LeS,
        Operator::I8x16LeU => SimdOp::I8x16LeU,
        Operator::I8x16GeS => SimdOp::I8x16GeS,
        Operator::I8x16GeU => SimdOp::I8x16GeU,
        Operator::I8x16Neg => SimdOp::I8x16Neg,
        Operator::I8x16Abs => SimdOp::I8x16Abs,
        Operator::I8x16Add => SimdOp::I8x16Add,
        Operator::I8x16AddSatS => SimdOp::I8x16AddSatS,
        Operator::I8x16AddSatU => SimdOp::I8x16AddSatU,
        Operator::I8x16Sub => SimdOp::I8x16Sub,
        Operator::I8x16SubSatS => SimdOp::I8x16SubSatS,
        Operator::I8x16SubSatU => SimdOp::I8x16SubSatU,
        Operator::I8x16Shl => SimdOp::I8x16Shl,
        Operator::I8x16ShrS => SimdOp::I8x16ShrS,
        Operator::I8x16ShrU => SimdOp::I8x16ShrU,

        Operator::I16x8AllTrue => SimdOp::I16x8AllTrue,
        Operator::I16x8Bitmask => SimdOp::I16x8Bitmask,
        Operator::I16x8Eq => SimdOp::I16x8Eq,
        Operator::I16x8Ne => SimdOp::I16x8Ne,
        Operator::I16x8Neg => SimdOp::I16x8Neg,
        Operator::I16x8Abs => SimdOp::I16x8Abs,
        Operator::I16x8Add => SimdOp::I16x8Add,
        Operator::I16x8Sub => SimdOp::I16x8Sub,
        Operator::I16x8Mul => SimdOp::I16x8Mul,
        Operator::I16x8Shl => SimdOp::I16x8Shl,
        Operator::I16x8ShrS => SimdOp::I16x8ShrS,
        Operator::I16x8ShrU => SimdOp::I16x8ShrU,

        Operator::I32x4AllTrue => SimdOp::I32x4AllTrue,
        Operator::I32x4Bitmask => SimdOp::I32x4Bitmask,
        Operator::I32x4Eq => SimdOp::I32x4Eq,
        Operator::I32x4Ne => SimdOp::I32x4Ne,
        Operator::I32x4LtS => SimdOp::I32x4LtS,
        Operator::I32x4LtU => SimdOp::I32x4LtU,
        Operator::I32x4GtS => SimdOp::I32x4GtS,
        Operator::I32x4GtU => SimdOp::I32x4GtU,
        Operator::I32x4LeS => SimdOp::I32x4LeS,
        Operator::I32x4LeU => SimdOp::I32x4LeU,
        Operator::I32x4GeS => SimdOp::I32x4GeS,
        Operator::I32x4GeU => SimdOp::I32x4GeU,
        Operator::I32x4Neg => SimdOp::I32x4Neg,
        Operator::I32x4Abs => SimdOp::I32x4Abs,
        Operator::I32x4Add => SimdOp::I32x4Add,
        Operator::I32x4Sub => SimdOp::I32x4Sub,
        Operator::I32x4Mul => SimdOp::I32x4Mul,
        Operator::I32x4Shl => SimdOp::I32x4Shl,
        Operator::I32x4ShrS => SimdOp::I32x4ShrS,
        Operator::I32x4ShrU => SimdOp::I32x4ShrU,

        Operator::I64x2AllTrue => SimdOp::I64x2AllTrue,
        Operator::I64x2Bitmask => SimdOp::I64x2Bitmask,
        Operator::I64x2Eq => SimdOp::I64x2Eq,
        Operator::I64x2Ne => SimdOp::I64x2Ne,
        Operator::I64x2Neg => SimdOp::I64x2Neg,
        Operator::I64x2Abs => SimdOp::I64x2Abs,
        Operator::I64x2Add => SimdOp::I64x2Add,
        Operator::I64x2Sub => SimdOp::I64x2Sub,
        Operator::I64x2Mul => SimdOp::I64x2Mul,
        Operator::I64x2Shl => SimdOp::I64x2Shl,
        Operator::I64x2ShrS => SimdOp::I64x2ShrS,
        Operator::I64x2ShrU => SimdOp::I64x2ShrU,

        Operator::F32x4Eq => SimdOp::F32x4Eq,
        Operator::F32x4Ne => SimdOp::F32x4Ne,
        Operator::F32x4Lt => SimdOp::F32x4Lt,
        Operator::F32x4Gt => SimdOp::F32x4Gt,
        Operator::F32x4Le => SimdOp::F32x4Le,
        Operator::F32x4Ge => SimdOp::F32x4Ge,
        Operator::F32x4Abs => SimdOp::F32x4Abs,
        Operator::F32x4Neg => SimdOp::F32x4Neg,
        Operator::F32x4Sqrt => SimdOp::F32x4Sqrt,
        Operator::F32x4Add => SimdOp::F32x4Add,
        Operator::F32x4Sub => SimdOp::F32x4Sub,
        Operator::F32x4Mul => SimdOp::F32x4Mul,
        Operator::F32x4Div => SimdOp::F32x4Div,
        Operator::F32x4Min => SimdOp::F32x4Min,
        Operator::F32x4Max => SimdOp::F32x4Max,

        Operator::F64x2Eq => SimdOp::F64x2Eq,
        Operator::F64x2Ne => SimdOp::F64x2Ne,
        Operator::F64x2Lt => SimdOp::F64x2Lt,
        Operator::F64x2Gt => SimdOp::F64x2Gt,
        Operator::F64x2Le => SimdOp::F64x2Le,
        Operator::F64x2Ge => SimdOp::F64x2Ge,
        Operator::F64x2Abs => SimdOp::F64x2Abs,
        Operator::F64x2Neg => SimdOp::F64x2Neg,
        Operator::F64x2Sqrt => SimdOp::F64x2Sqrt,
        Operator::F64x2Add => SimdOp::F64x2Add,
        Operator::F64x2Sub => SimdOp::F64x2Sub,
        Operator::F64x2Mul => SimdOp::F64x2Mul,
        Operator::F64x2Div => SimdOp::F64x2Div,
        Operator::F64x2Min => SimdOp::F64x2Min,
        Operator::F64x2Max => SimdOp::F64x2Max,

        _ => return None,
    })
}

#[inline]
fn pop_i32(stack: &mut Vec<WasmVal>) -> i32 {
    match stack.pop().expect("validated stack") {
        WasmVal::I32(v) => v,
        _ => unreachable!("validated i32"),
    }
}

#[inline]
fn pop_i64(stack: &mut Vec<WasmVal>) -> i64 {
    match stack.pop().expect("validated stack") {
        WasmVal::I64(v) => v,
        _ => unreachable!("validated i64"),
    }
}

#[inline]
fn pop_f32(stack: &mut Vec<WasmVal>) -> u32 {
    match stack.pop().expect("validated stack") {
        WasmVal::F32(v) => v,
        _ => unreachable!("validated f32"),
    }
}

#[inline]
fn pop_f64(stack: &mut Vec<WasmVal>) -> u64 {
    match stack.pop().expect("validated stack") {
        WasmVal::F64(v) => v,
        _ => unreachable!("validated f64"),
    }
}

#[inline]
fn pop_v128(stack: &mut Vec<WasmVal>) -> u128 {
    match stack.pop().expect("validated stack") {
        WasmVal::V128(v) => v,
        _ => unreachable!("validated v128"),
    }
}

fn mem_slice(
    memories: &[Vec<u8>],
    imm: MemImmediate,
    base: i32,
    len: usize,
) -> Result<&[u8], WasmTrap> {
    let mem = memories
        .get(imm.mem as usize)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let addr = (base as u32 as u64)
        .checked_add(imm.offset)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let end = addr
        .checked_add(len as u64)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    if end > mem.len() as u64 {
        return Err(WasmTrap::OutOfBoundsMemoryAccess);
    }
    Ok(&mem[addr as usize..end as usize])
}

fn mem_slice_mut(
    memories: &mut [Vec<u8>],
    imm: MemImmediate,
    base: i32,
    len: usize,
) -> Result<&mut [u8], WasmTrap> {
    let mem = memories
        .get_mut(imm.mem as usize)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let addr = (base as u32 as u64)
        .checked_add(imm.offset)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    let end = addr
        .checked_add(len as u64)
        .ok_or(WasmTrap::OutOfBoundsMemoryAccess)?;
    if end > mem.len() as u64 {
        return Err(WasmTrap::OutOfBoundsMemoryAccess);
    }
    Ok(&mut mem[addr as usize..end as usize])
}

fn map_i8x16_bin(a: u128, b: u128, f: impl Fn(u8, u8) -> u8) -> u128 {
    let ab = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = f(ab[i], bb[i]);
    }
    u128::from_le_bytes(out)
}

fn map_i16x8_bin(a: u128, b: u128, f: impl Fn(i16, i16) -> i16) -> u128 {
    let ab = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..8 {
        let la = i16::from_le_bytes([ab[i * 2], ab[i * 2 + 1]]);
        let lb = i16::from_le_bytes([bb[i * 2], bb[i * 2 + 1]]);
        let r = f(la, lb).to_le_bytes();
        out[i * 2] = r[0];
        out[i * 2 + 1] = r[1];
    }
    u128::from_le_bytes(out)
}

fn map_i32x4_bin(a: u128, b: u128, f: impl Fn(i32, i32) -> i32) -> u128 {
    let ab = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..4 {
        let o = i * 4;
        let la = i32::from_le_bytes([ab[o], ab[o + 1], ab[o + 2], ab[o + 3]]);
        let lb = i32::from_le_bytes([bb[o], bb[o + 1], bb[o + 2], bb[o + 3]]);
        out[o..o + 4].copy_from_slice(&f(la, lb).to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn map_i64x2_bin(a: u128, b: u128, f: impl Fn(i64, i64) -> i64) -> u128 {
    let ab = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..2 {
        let o = i * 8;
        let la = i64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
        let lb = i64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
        out[o..o + 8].copy_from_slice(&f(la, lb).to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn map_f32x4_bin(a: u128, b: u128, f: impl Fn(f32, f32) -> f32) -> u128 {
    let ab = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..4 {
        let o = i * 4;
        let la = f32::from_le_bytes([ab[o], ab[o + 1], ab[o + 2], ab[o + 3]]);
        let lb = f32::from_le_bytes([bb[o], bb[o + 1], bb[o + 2], bb[o + 3]]);
        out[o..o + 4].copy_from_slice(&f(la, lb).to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn map_f64x2_bin(a: u128, b: u128, f: impl Fn(f64, f64) -> f64) -> u128 {
    let ab = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let mut out = [0u8; 16];
    for i in 0..2 {
        let o = i * 8;
        let la = f64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
        let lb = f64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
        out[o..o + 8].copy_from_slice(&f(la, lb).to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

#[inline]
fn wasm_f32_min(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_negative() || b.is_sign_negative()) {
        -0.0
    } else {
        a.min(b)
    }
}

#[inline]
fn wasm_f32_max(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_positive() || b.is_sign_positive()) {
        0.0
    } else {
        a.max(b)
    }
}

#[inline]
fn wasm_f64_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_negative() || b.is_sign_negative()) {
        -0.0
    } else {
        a.min(b)
    }
}

#[inline]
fn wasm_f64_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 && (a.is_sign_positive() || b.is_sign_positive()) {
        0.0
    } else {
        a.max(b)
    }
}

pub(crate) fn exec_simd(
    op: &SimdOp,
    stack: &mut Vec<WasmVal>,
    memories: &mut [Vec<u8>],
) -> Result<(), WasmTrap> {
    match *op {
        SimdOp::V128Const(v) => stack.push(WasmVal::V128(v)),
        SimdOp::V128Load(imm) => {
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 16)?;
            stack.push(WasmVal::V128(u128::from_le_bytes(s.try_into().unwrap())));
        }
        SimdOp::V128Load8Splat(imm) => {
            let base = pop_i32(stack);
            let b = mem_slice(memories, imm, base, 1)?[0];
            stack.push(WasmVal::V128(u128::from_le_bytes([b; 16])));
        }
        SimdOp::V128Load16Splat(imm) => {
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 2)?;
            let mut out = [0u8; 16];
            for i in 0..8 {
                out[i * 2] = s[0];
                out[i * 2 + 1] = s[1];
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::V128Load32Splat(imm) => {
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 4)?;
            let mut out = [0u8; 16];
            for i in 0..4 {
                out[i * 4..i * 4 + 4].copy_from_slice(s);
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::V128Load64Splat(imm) => {
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 8)?;
            let mut out = [0u8; 16];
            out[0..8].copy_from_slice(s);
            out[8..16].copy_from_slice(s);
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::V128Load32Zero(imm) => {
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 4)?;
            let mut out = [0u8; 16];
            out[0..4].copy_from_slice(s);
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::V128Load64Zero(imm) => {
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 8)?;
            let mut out = [0u8; 16];
            out[0..8].copy_from_slice(s);
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::V128Load8Lane(imm, lane) => {
            let mut vec_bytes = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            vec_bytes[lane as usize] = mem_slice(memories, imm, base, 1)?[0];
            stack.push(WasmVal::V128(u128::from_le_bytes(vec_bytes)));
        }
        SimdOp::V128Load16Lane(imm, lane) => {
            let mut vec_bytes = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 2)?;
            let o = (lane as usize) * 2;
            vec_bytes[o..o + 2].copy_from_slice(s);
            stack.push(WasmVal::V128(u128::from_le_bytes(vec_bytes)));
        }
        SimdOp::V128Load32Lane(imm, lane) => {
            let mut vec_bytes = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 4)?;
            let o = (lane as usize) * 4;
            vec_bytes[o..o + 4].copy_from_slice(s);
            stack.push(WasmVal::V128(u128::from_le_bytes(vec_bytes)));
        }
        SimdOp::V128Load64Lane(imm, lane) => {
            let mut vec_bytes = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            let s = mem_slice(memories, imm, base, 8)?;
            let o = (lane as usize) * 8;
            vec_bytes[o..o + 8].copy_from_slice(s);
            stack.push(WasmVal::V128(u128::from_le_bytes(vec_bytes)));
        }
        SimdOp::V128Store(imm) => {
            let val = pop_v128(stack);
            let base = pop_i32(stack);
            mem_slice_mut(memories, imm, base, 16)?.copy_from_slice(&val.to_le_bytes());
        }
        SimdOp::V128Store8Lane(imm, lane) => {
            let vb = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            mem_slice_mut(memories, imm, base, 1)?[0] = vb[lane as usize];
        }
        SimdOp::V128Store16Lane(imm, lane) => {
            let vb = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            let o = (lane as usize) * 2;
            mem_slice_mut(memories, imm, base, 2)?.copy_from_slice(&vb[o..o + 2]);
        }
        SimdOp::V128Store32Lane(imm, lane) => {
            let vb = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            let o = (lane as usize) * 4;
            mem_slice_mut(memories, imm, base, 4)?.copy_from_slice(&vb[o..o + 4]);
        }
        SimdOp::V128Store64Lane(imm, lane) => {
            let vb = pop_v128(stack).to_le_bytes();
            let base = pop_i32(stack);
            let o = (lane as usize) * 8;
            mem_slice_mut(memories, imm, base, 8)?.copy_from_slice(&vb[o..o + 8]);
        }

        SimdOp::I8x16Shuffle(lanes) => {
            let b = pop_v128(stack).to_le_bytes();
            let a = pop_v128(stack).to_le_bytes();
            let mut out = [0u8; 16];
            for (i, &idx) in lanes.iter().enumerate() {
                out[i] = if idx < 16 {
                    a[idx as usize]
                } else {
                    b[(idx - 16) as usize]
                };
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::I8x16Swizzle => {
            let idx = pop_v128(stack).to_le_bytes();
            let src = pop_v128(stack).to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                let s = idx[i];
                out[i] = if s < 16 { src[s as usize] } else { 0 };
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }

        SimdOp::I8x16Splat => {
            let v = pop_i32(stack) as u8;
            stack.push(WasmVal::V128(u128::from_le_bytes([v; 16])));
        }
        SimdOp::I16x8Splat => {
            let b = (pop_i32(stack) as i16).to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..8 {
                out[i * 2] = b[0];
                out[i * 2 + 1] = b[1];
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::I32x4Splat => {
            let b = pop_i32(stack).to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                out[i * 4..i * 4 + 4].copy_from_slice(&b);
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::I64x2Splat => {
            let b = pop_i64(stack).to_le_bytes();
            let mut out = [0u8; 16];
            out[0..8].copy_from_slice(&b);
            out[8..16].copy_from_slice(&b);
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F32x4Splat => {
            let b = pop_f32(stack).to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                out[i * 4..i * 4 + 4].copy_from_slice(&b);
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F64x2Splat => {
            let b = pop_f64(stack).to_le_bytes();
            let mut out = [0u8; 16];
            out[0..8].copy_from_slice(&b);
            out[8..16].copy_from_slice(&b);
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }

        SimdOp::I8x16ExtractLaneS(l) => {
            let b = pop_v128(stack).to_le_bytes();
            stack.push(WasmVal::I32(b[l as usize] as i8 as i32));
        }
        SimdOp::I8x16ExtractLaneU(l) => {
            let b = pop_v128(stack).to_le_bytes();
            stack.push(WasmVal::I32(b[l as usize] as i32));
        }
        SimdOp::I8x16ReplaceLane(l) => {
            let val = pop_i32(stack) as u8;
            let mut b = pop_v128(stack).to_le_bytes();
            b[l as usize] = val;
            stack.push(WasmVal::V128(u128::from_le_bytes(b)));
        }
        SimdOp::I16x8ExtractLaneS(l) => {
            let b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 2;
            stack.push(WasmVal::I32(i16::from_le_bytes([b[o], b[o + 1]]) as i32));
        }
        SimdOp::I16x8ExtractLaneU(l) => {
            let b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 2;
            stack.push(WasmVal::I32(u16::from_le_bytes([b[o], b[o + 1]]) as i32));
        }
        SimdOp::I16x8ReplaceLane(l) => {
            let val = (pop_i32(stack) as i16).to_le_bytes();
            let mut b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 2;
            b[o..o + 2].copy_from_slice(&val);
            stack.push(WasmVal::V128(u128::from_le_bytes(b)));
        }
        SimdOp::I32x4ExtractLane(l) => {
            let b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 4;
            stack.push(WasmVal::I32(i32::from_le_bytes(
                b[o..o + 4].try_into().unwrap(),
            )));
        }
        SimdOp::I32x4ReplaceLane(l) => {
            let val = pop_i32(stack).to_le_bytes();
            let mut b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 4;
            b[o..o + 4].copy_from_slice(&val);
            stack.push(WasmVal::V128(u128::from_le_bytes(b)));
        }
        SimdOp::I64x2ExtractLane(l) => {
            let b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 8;
            stack.push(WasmVal::I64(i64::from_le_bytes(
                b[o..o + 8].try_into().unwrap(),
            )));
        }
        SimdOp::I64x2ReplaceLane(l) => {
            let val = pop_i64(stack).to_le_bytes();
            let mut b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 8;
            b[o..o + 8].copy_from_slice(&val);
            stack.push(WasmVal::V128(u128::from_le_bytes(b)));
        }
        SimdOp::F32x4ExtractLane(l) => {
            let b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 4;
            stack.push(WasmVal::F32(u32::from_le_bytes(
                b[o..o + 4].try_into().unwrap(),
            )));
        }
        SimdOp::F32x4ReplaceLane(l) => {
            let val = pop_f32(stack).to_le_bytes();
            let mut b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 4;
            b[o..o + 4].copy_from_slice(&val);
            stack.push(WasmVal::V128(u128::from_le_bytes(b)));
        }
        SimdOp::F64x2ExtractLane(l) => {
            let b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 8;
            stack.push(WasmVal::F64(u64::from_le_bytes(
                b[o..o + 8].try_into().unwrap(),
            )));
        }
        SimdOp::F64x2ReplaceLane(l) => {
            let val = pop_f64(stack).to_le_bytes();
            let mut b = pop_v128(stack).to_le_bytes();
            let o = (l as usize) * 8;
            b[o..o + 8].copy_from_slice(&val);
            stack.push(WasmVal::V128(u128::from_le_bytes(b)));
        }

        SimdOp::V128Not => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(!a));
        }
        SimdOp::V128And => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(a & b));
        }
        SimdOp::V128AndNot => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(a & !b));
        }
        SimdOp::V128Or => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(a | b));
        }
        SimdOp::V128Xor => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(a ^ b));
        }
        SimdOp::V128Bitselect => {
            let mask = pop_v128(stack);
            let v2 = pop_v128(stack);
            let v1 = pop_v128(stack);
            stack.push(WasmVal::V128((v1 & mask) | (v2 & !mask)));
        }
        SimdOp::V128AnyTrue => {
            let a = pop_v128(stack);
            stack.push(WasmVal::I32(i32::from(a != 0)));
        }

        SimdOp::I8x16AllTrue => {
            let b = pop_v128(stack).to_le_bytes();
            stack.push(WasmVal::I32(i32::from(b.iter().all(|&x| x != 0))));
        }
        SimdOp::I8x16Bitmask => {
            let b = pop_v128(stack).to_le_bytes();
            let mut m = 0i32;
            for (i, &x) in b.iter().enumerate() {
                if (x as i8) < 0 {
                    m |= 1 << i;
                }
            }
            stack.push(WasmVal::I32(m));
        }
        SimdOp::I8x16Eq => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if x == y { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16Ne => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if x != y { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16LtS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if (x as i8) < (y as i8) { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16LtU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if x < y { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16GtS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if (x as i8) > (y as i8) { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16GtU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if x > y { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16LeS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if (x as i8) <= (y as i8) { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16LeU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if x <= y { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16GeS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if (x as i8) >= (y as i8) { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16GeU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                if x >= y { 0xff } else { 0 }
            })));
        }
        SimdOp::I8x16Neg => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, 0, |x, _| {
                (x as i8).wrapping_neg() as u8
            })));
        }
        SimdOp::I8x16Abs => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, 0, |x, _| {
                (x as i8).wrapping_abs() as u8
            })));
        }
        SimdOp::I8x16Add => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, u8::wrapping_add)));
        }
        SimdOp::I8x16AddSatS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                (x as i8).saturating_add(y as i8) as u8
            })));
        }
        SimdOp::I8x16AddSatU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, u8::saturating_add)));
        }
        SimdOp::I8x16Sub => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, u8::wrapping_sub)));
        }
        SimdOp::I8x16SubSatS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, |x, y| {
                (x as i8).saturating_sub(y as i8) as u8
            })));
        }
        SimdOp::I8x16SubSatU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, b, u8::saturating_sub)));
        }
        SimdOp::I8x16Shl => {
            let s = (pop_i32(stack) as u32) & 7;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, 0, |x, _| x << s)));
        }
        SimdOp::I8x16ShrS => {
            let s = (pop_i32(stack) as u32) & 7;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, 0, |x, _| {
                ((x as i8) >> s) as u8
            })));
        }
        SimdOp::I8x16ShrU => {
            let s = (pop_i32(stack) as u32) & 7;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i8x16_bin(a, 0, |x, _| x >> s)));
        }

        SimdOp::I16x8AllTrue => {
            let ab = pop_v128(stack).to_le_bytes();
            let all = (0..8).all(|i| u16::from_le_bytes([ab[i * 2], ab[i * 2 + 1]]) != 0);
            stack.push(WasmVal::I32(i32::from(all)));
        }
        SimdOp::I16x8Bitmask => {
            let ab = pop_v128(stack).to_le_bytes();
            let mut m = 0i32;
            for i in 0..8 {
                if i16::from_le_bytes([ab[i * 2], ab[i * 2 + 1]]) < 0 {
                    m |= 1 << i;
                }
            }
            stack.push(WasmVal::I32(m));
        }
        SimdOp::I16x8Eq => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, b, |x, y| {
                if x == y { -1 } else { 0 }
            })));
        }
        SimdOp::I16x8Ne => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, b, |x, y| {
                if x != y { -1 } else { 0 }
            })));
        }
        SimdOp::I16x8Neg => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, 0, |x, _| x.wrapping_neg())));
        }
        SimdOp::I16x8Abs => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, 0, |x, _| x.wrapping_abs())));
        }
        SimdOp::I16x8Add => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, b, i16::wrapping_add)));
        }
        SimdOp::I16x8Sub => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, b, i16::wrapping_sub)));
        }
        SimdOp::I16x8Mul => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, b, i16::wrapping_mul)));
        }
        SimdOp::I16x8Shl => {
            let s = (pop_i32(stack) as u32) & 15;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, 0, |x, _| x << s)));
        }
        SimdOp::I16x8ShrS => {
            let s = (pop_i32(stack) as u32) & 15;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, 0, |x, _| x >> s)));
        }
        SimdOp::I16x8ShrU => {
            let s = (pop_i32(stack) as u32) & 15;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i16x8_bin(a, 0, |x, _| {
                ((x as u16) >> s) as i16
            })));
        }

        SimdOp::I32x4AllTrue => {
            let ab = pop_v128(stack).to_le_bytes();
            let all = (0..4).all(|i| {
                let o = i * 4;
                u32::from_le_bytes(ab[o..o + 4].try_into().unwrap()) != 0
            });
            stack.push(WasmVal::I32(i32::from(all)));
        }
        SimdOp::I32x4Bitmask => {
            let ab = pop_v128(stack).to_le_bytes();
            let mut m = 0i32;
            for i in 0..4 {
                let o = i * 4;
                if i32::from_le_bytes(ab[o..o + 4].try_into().unwrap()) < 0 {
                    m |= 1 << i;
                }
            }
            stack.push(WasmVal::I32(m));
        }
        SimdOp::I32x4Eq => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if x == y { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4Ne => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if x != y { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4LtS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if x < y { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4LtU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if (x as u32) < (y as u32) { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4GtS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if x > y { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4GtU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if (x as u32) > (y as u32) { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4LeS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if x <= y { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4LeU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if (x as u32) <= (y as u32) { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4GeS => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if x >= y { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4GeU => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, |x, y| {
                if (x as u32) >= (y as u32) { -1 } else { 0 }
            })));
        }
        SimdOp::I32x4Neg => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, 0, |x, _| x.wrapping_neg())));
        }
        SimdOp::I32x4Abs => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, 0, |x, _| x.wrapping_abs())));
        }
        SimdOp::I32x4Add => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, i32::wrapping_add)));
        }
        SimdOp::I32x4Sub => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, i32::wrapping_sub)));
        }
        SimdOp::I32x4Mul => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, b, i32::wrapping_mul)));
        }
        SimdOp::I32x4Shl => {
            let s = (pop_i32(stack) as u32) & 31;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, 0, |x, _| x << s)));
        }
        SimdOp::I32x4ShrS => {
            let s = (pop_i32(stack) as u32) & 31;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, 0, |x, _| x >> s)));
        }
        SimdOp::I32x4ShrU => {
            let s = (pop_i32(stack) as u32) & 31;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i32x4_bin(a, 0, |x, _| {
                ((x as u32) >> s) as i32
            })));
        }

        SimdOp::I64x2AllTrue => {
            let ab = pop_v128(stack).to_le_bytes();
            let all = u64::from_le_bytes(ab[0..8].try_into().unwrap()) != 0
                && u64::from_le_bytes(ab[8..16].try_into().unwrap()) != 0;
            stack.push(WasmVal::I32(i32::from(all)));
        }
        SimdOp::I64x2Bitmask => {
            let ab = pop_v128(stack).to_le_bytes();
            let l0 = i64::from_le_bytes(ab[0..8].try_into().unwrap()) < 0;
            let l1 = i64::from_le_bytes(ab[8..16].try_into().unwrap()) < 0;
            stack.push(WasmVal::I32(i32::from(l0) | (i32::from(l1) << 1)));
        }
        SimdOp::I64x2Eq => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, b, |x, y| {
                if x == y { -1 } else { 0 }
            })));
        }
        SimdOp::I64x2Ne => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, b, |x, y| {
                if x != y { -1 } else { 0 }
            })));
        }
        SimdOp::I64x2Neg => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, 0, |x, _| x.wrapping_neg())));
        }
        SimdOp::I64x2Abs => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, 0, |x, _| x.wrapping_abs())));
        }
        SimdOp::I64x2Add => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, b, i64::wrapping_add)));
        }
        SimdOp::I64x2Sub => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, b, i64::wrapping_sub)));
        }
        SimdOp::I64x2Mul => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, b, i64::wrapping_mul)));
        }
        SimdOp::I64x2Shl => {
            let s = (pop_i32(stack) as u32) & 63;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, 0, |x, _| x << s)));
        }
        SimdOp::I64x2ShrS => {
            let s = (pop_i32(stack) as u32) & 63;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, 0, |x, _| x >> s)));
        }
        SimdOp::I64x2ShrU => {
            let s = (pop_i32(stack) as u32) & 63;
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_i64x2_bin(a, 0, |x, _| {
                ((x as u64) >> s) as i64
            })));
        }

        SimdOp::F32x4Eq => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                let o = i * 4;
                let la = f32::from_le_bytes(ab[o..o + 4].try_into().unwrap());
                let lb = f32::from_le_bytes(bb[o..o + 4].try_into().unwrap());
                let m: u32 = if la == lb { u32::MAX } else { 0 };
                out[o..o + 4].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F32x4Ne => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                let o = i * 4;
                let la = f32::from_le_bytes(ab[o..o + 4].try_into().unwrap());
                let lb = f32::from_le_bytes(bb[o..o + 4].try_into().unwrap());
                let m: u32 = if la != lb { u32::MAX } else { 0 };
                out[o..o + 4].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F32x4Lt => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                let o = i * 4;
                let la = f32::from_le_bytes(ab[o..o + 4].try_into().unwrap());
                let lb = f32::from_le_bytes(bb[o..o + 4].try_into().unwrap());
                let m: u32 = if la < lb { u32::MAX } else { 0 };
                out[o..o + 4].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F32x4Gt => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                let o = i * 4;
                let la = f32::from_le_bytes(ab[o..o + 4].try_into().unwrap());
                let lb = f32::from_le_bytes(bb[o..o + 4].try_into().unwrap());
                let m: u32 = if la > lb { u32::MAX } else { 0 };
                out[o..o + 4].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F32x4Le => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                let o = i * 4;
                let la = f32::from_le_bytes(ab[o..o + 4].try_into().unwrap());
                let lb = f32::from_le_bytes(bb[o..o + 4].try_into().unwrap());
                let m: u32 = if la <= lb { u32::MAX } else { 0 };
                out[o..o + 4].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F32x4Ge => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..4 {
                let o = i * 4;
                let la = f32::from_le_bytes(ab[o..o + 4].try_into().unwrap());
                let lb = f32::from_le_bytes(bb[o..o + 4].try_into().unwrap());
                let m: u32 = if la >= lb { u32::MAX } else { 0 };
                out[o..o + 4].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F32x4Abs => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, 0, |x, _| x.abs())));
        }
        SimdOp::F32x4Neg => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, 0, |x, _| -x)));
        }
        SimdOp::F32x4Sqrt => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, 0, |x, _| x.sqrt())));
        }
        SimdOp::F32x4Add => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, b, |x, y| x + y)));
        }
        SimdOp::F32x4Sub => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, b, |x, y| x - y)));
        }
        SimdOp::F32x4Mul => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, b, |x, y| x * y)));
        }
        SimdOp::F32x4Div => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, b, |x, y| x / y)));
        }
        SimdOp::F32x4Min => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, b, wasm_f32_min)));
        }
        SimdOp::F32x4Max => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f32x4_bin(a, b, wasm_f32_max)));
        }

        SimdOp::F64x2Eq => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..2 {
                let o = i * 8;
                let la = f64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
                let lb = f64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
                let m: u64 = if la == lb { u64::MAX } else { 0 };
                out[o..o + 8].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F64x2Ne => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..2 {
                let o = i * 8;
                let la = f64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
                let lb = f64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
                let m: u64 = if la != lb { u64::MAX } else { 0 };
                out[o..o + 8].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F64x2Lt => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..2 {
                let o = i * 8;
                let la = f64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
                let lb = f64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
                let m: u64 = if la < lb { u64::MAX } else { 0 };
                out[o..o + 8].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F64x2Gt => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..2 {
                let o = i * 8;
                let la = f64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
                let lb = f64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
                let m: u64 = if la > lb { u64::MAX } else { 0 };
                out[o..o + 8].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F64x2Le => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..2 {
                let o = i * 8;
                let la = f64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
                let lb = f64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
                let m: u64 = if la <= lb { u64::MAX } else { 0 };
                out[o..o + 8].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F64x2Ge => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            let ab = a.to_le_bytes();
            let bb = b.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..2 {
                let o = i * 8;
                let la = f64::from_le_bytes(ab[o..o + 8].try_into().unwrap());
                let lb = f64::from_le_bytes(bb[o..o + 8].try_into().unwrap());
                let m: u64 = if la >= lb { u64::MAX } else { 0 };
                out[o..o + 8].copy_from_slice(&m.to_le_bytes());
            }
            stack.push(WasmVal::V128(u128::from_le_bytes(out)));
        }
        SimdOp::F64x2Abs => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, 0, |x, _| x.abs())));
        }
        SimdOp::F64x2Neg => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, 0, |x, _| -x)));
        }
        SimdOp::F64x2Sqrt => {
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, 0, |x, _| x.sqrt())));
        }
        SimdOp::F64x2Add => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, b, |x, y| x + y)));
        }
        SimdOp::F64x2Sub => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, b, |x, y| x - y)));
        }
        SimdOp::F64x2Mul => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, b, |x, y| x * y)));
        }
        SimdOp::F64x2Div => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, b, |x, y| x / y)));
        }
        SimdOp::F64x2Min => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, b, wasm_f64_min)));
        }
        SimdOp::F64x2Max => {
            let b = pop_v128(stack);
            let a = pop_v128(stack);
            stack.push(WasmVal::V128(map_f64x2_bin(a, b, wasm_f64_max)));
        }
    }
    Ok(())
}
