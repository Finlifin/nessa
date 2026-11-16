//! Embedded WebAssembly runtime helper functions and memory layout for `aot.rs`.
//!
//! All Nessa values in AOT-compiled WebAssembly use Nessa's canonical 64-bit
//! `TaggedValue` representation (`i64`), while heap pointers are 8-byte-aligned
//! 32-bit offsets (`>= STATIC_DATA_BASE`, `ptr & 7 == 0`) inside linear `memory 0`.

use wasm_encoder::{
    BlockType, CodeSection, Function, FunctionSection, MemArg, TypeSection, ValType,
};

use crate::aot::WasmTarget;

// ── TaggedValue bit layout (matching `engine/runtime/src/lib.rs`) ──────────
pub(crate) const TAG_MASK: i64 = 0b111;
pub(crate) const ROT_F64_TAG_MASK: i64 = 0b110;
pub(crate) const IMM_LOW7_MASK: i64 = 0x7f;

pub(crate) const IMM_I_SMALL_TAG: i64 = 0x01; // TAG_IMM | (0x0 << 3)
pub(crate) const IMM_U_SMALL_TAG: i64 = 0x09; // TAG_IMM | (0x1 << 3)
pub(crate) const IMM_F64_TAG: i64 = 0x11; // TAG_IMM | (0x2 << 3)
pub(crate) const IMM_BOOL_TAG: i64 = 0x19; // TAG_IMM | (0x3 << 3)
pub(crate) const IMM_NULL_TAG: i64 = 0x29; // TAG_IMM | (0x5 << 3)
pub(crate) const IMM_UNIT_TAG: i64 = 0x31; // TAG_IMM | (0x6 << 3)
pub(crate) const IMM_CHAR_TAG: i64 = 0x39; // TAG_IMM | (0x7 << 3)
pub(crate) const IMM_TYPE_TAG: i64 = 0x41; // TAG_IMM | (0x8 << 3)
pub(crate) const IMM_ENUM_TAG: i64 = 0x49; // TAG_IMM | (0x9 << 3)
pub(crate) const IMM_TRAIT_PROOF_TAG: i64 = 0x51; // TAG_IMM | (0xA << 3)
pub(crate) const IMM_USIZE_TAG: i64 = 0x59; // TAG_IMM | (0xB << 3)
pub(crate) const IMM_ISIZE_TAG: i64 = 0x61; // TAG_IMM | (0xC << 3)
pub(crate) const IMM_NARROW_TAG: i64 = 0x69; // TAG_IMM | (0xD << 3)

pub(crate) const VAL_FALSE: i64 = 0x19;
pub(crate) const VAL_TRUE: i64 = 0x99;
pub(crate) const VAL_NULL: i64 = 0x29;
pub(crate) const VAL_UNIT: i64 = 0x31;

// Narrow sub-kinds in bits [35:32] of `IMM_NARROW` data (`val >> 7`)
pub(crate) const NARROW_U8: i64 = 0;
pub(crate) const NARROW_U16: i64 = 1;
pub(crate) const NARROW_U32: i64 = 2;
pub(crate) const NARROW_I8: i64 = 3;
pub(crate) const NARROW_I16: i64 = 4;
pub(crate) const NARROW_I32: i64 = 5;
pub(crate) const NARROW_F32: i64 = 6;

// Heap object header roles at `payload_ptr - 12`:
//   [ptr - 16]: type_index: u32
//   [ptr - 12]: role: u32
//   [ptr -  8]: len_or_words: u32
//   [ptr -  4]: aux: u32
pub(crate) const ROLE_ORDINARY: i32 = 0;
pub(crate) const ROLE_ERROR_ENVELOPE: i32 = 1;
pub(crate) const ROLE_STRING_UTF8: i32 = 2;
pub(crate) const ROLE_BYTES: i32 = 3;
pub(crate) const ROLE_BOXED_SCALAR: i32 = 4;

// Well-known Intrinsic TypeIndex values (`0..23`)
pub(crate) const TY_U8: u32 = 0;
pub(crate) const TY_U16: u32 = 1;
pub(crate) const TY_U32: u32 = 2;
pub(crate) const TY_U64: u32 = 3;
pub(crate) const TY_USIZE: u32 = 5;
pub(crate) const TY_I8: u32 = 6;
pub(crate) const TY_I16: u32 = 7;
pub(crate) const TY_I32: u32 = 8;
pub(crate) const TY_I64: u32 = 9;
pub(crate) const TY_ISIZE: u32 = 11;
pub(crate) const TY_F32: u32 = 12;
pub(crate) const TY_F64: u32 = 13;
pub(crate) const TY_BOOL: u32 = 14;
pub(crate) const TY_CHAR: u32 = 15;
pub(crate) const TY_STR: u32 = 16;
pub(crate) const TY_UNIT: u32 = 17;
pub(crate) const TY_ANY: u32 = 18;
pub(crate) const TY_NORETURN: u32 = 19;
pub(crate) const TY_TYPE: u32 = 20;
pub(crate) const TY_CLOSURE: u32 = 21;

// Linear memory fixed regions
pub(crate) const WASI_IOVEC_ADDR: u64 = 0x0040; // 8 bytes: { ptr: u32, len: u32 }
pub(crate) const WASI_NWRITTEN_ADDR: u64 = 0x0048; // 4 bytes
pub(crate) const WASI_CLOCK_ADDR: u64 = 0x0050; // 8 bytes
pub(crate) const FMT_SCRATCH_ADDR: i32 = 0x0060; // 128 bytes [0x0060..0x00E0)
pub(crate) const NEWLINE_BYTE_ADDR: i32 = 0x00E0; // 1 byte '\n'
pub(crate) const REG_PASS_BASE: u64 = 0x0100; // 32 * 8 = 256 bytes [0x0100..0x0200)
pub(crate) const SLOT_STACK_BASE: i32 = 0x0200; // [0x0200..0x20200) = 128 KiB
pub(crate) const SLOT_STACK_LIMIT: i32 = 0x20200;
pub(crate) const STATIC_DATA_BASE: u32 = 0x20200;

// Module globals
pub(crate) const GLOBAL_HEAP_PTR: u32 = 0;
pub(crate) const GLOBAL_SLOT_SP: u32 = 1;
pub(crate) const GLOBAL_INITIALIZED: u32 = 2;
pub(crate) const GLOBAL_RNG_STATE: u32 = 3;

// TypeMetadata kind tags in static table (32 bytes per TypeIndex):
//   +0: canon_idx: u32
//   +4: kind_tag: u32
//   +8: count_or_inner: u32
//  +12: aux: u32
//  +16: type_id_hi: u64
//  +24: type_id_lo: u64
pub(crate) const TK_INTRINSIC: u32 = 0;
pub(crate) const TK_STRUCT: u32 = 1;
pub(crate) const TK_TUPLE: u32 = 2;
pub(crate) const TK_ENUM: u32 = 3;
pub(crate) const TK_NEWTYPE: u32 = 4;
pub(crate) const TK_OPTIONAL: u32 = 5;
pub(crate) const TK_ERROR: u32 = 6;
pub(crate) const TK_TRAIT: u32 = 7;
pub(crate) const TK_FUNCTION: u32 = 8;
pub(crate) const TK_OTHER: u32 = 9;

#[derive(Debug, Clone, Copy)]
pub(crate) struct StaticLayoutInfo {
    pub null_type_idx: u32,
    pub list_type_idx: u32,
    pub map_type_idx: u32,
    pub bytes_type_idx: u32,
    pub type_meta_addr: u32,
    pub type_count: u32,
    pub method_table_addr: u32,
    pub method_count: u32,
    pub trait_impl_addr: u32,
    pub trait_impl_count: u32,
    pub vtable_entries_addr: u32,
    pub wasi_fd_write: Option<u32>,
    pub wasi_proc_exit: Option<u32>,
    pub wasi_clock_time_get: Option<u32>,
    pub wasi_random_get: Option<u32>,
    pub wasi_args_sizes_get: Option<u32>,
    pub wasi_args_get: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RtFuncIds {
    pub nessa_fn_type: u32,
    pub alloc: u32,
    pub alloc_obj: u32,
    pub alloc_str: u32,
    pub str_concat: u32,
    pub str_eq: u32,
    pub val_eq: u32,
    pub to_i64: u32,
    pub from_i64: u32,
    pub to_f64: u32,
    pub from_f64: u32,
    pub int_to_str: u32,
    pub f64_to_str: u32,
    pub to_string: u32,
    pub write_str: u32,
    pub add: u32,
    pub sub: u32,
    pub mul: u32,
    pub div: u32,
    pub rem: u32,
    pub neg: u32,
    pub cmp: u32,
    pub list_new: u32,
    pub list_push: u32,
    pub list_pop: u32,
    pub list_get: u32,
    pub list_set: u32,
    pub map_new: u32,
    pub map_set: u32,
    pub map_get: u32,
    pub map_contains: u32,
    pub map_remove: u32,
    pub map_keys: u32,
    pub bytes_new: u32,
    pub bytes_from_str: u32,
    pub bytes_to_str: u32,
    pub bytes_get: u32,
    pub bytes_set: u32,
    pub bytes_push: u32,
    pub reflected_type: u32,
    pub find_method: u32,
    pub trait_proof: u32,
    pub trait_vtable_func: u32,
    pub type_check: u32,
    pub type_cast: u32,
    pub trap_unreachable: u32,
}

#[inline]
pub(crate) const fn mem(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

/// Encode a `TaggedValue` constant at compile time into raw `u64` bits.
pub(crate) fn encode_i64_imm(v: i64) -> Option<u64> {
    if (-(1_i64 << 56)..(1_i64 << 56)).contains(&v) {
        let data = (v as u64) & ((1_u64 << 57) - 1);
        Some((IMM_I_SMALL_TAG as u64) | (data << 7))
    } else {
        None
    }
}

pub(crate) fn encode_u64_imm(v: u64) -> Option<u64> {
    if v < (1_u64 << 57) {
        Some((IMM_U_SMALL_TAG as u64) | (v << 7))
    } else {
        None
    }
}

pub(crate) fn encode_f64_imm(v: f64) -> u64 {
    let bits = v.to_bits();
    let rot = bits.rotate_left(4);
    if (rot & (ROT_F64_TAG_MASK as u64)) != 0 {
        rot
    } else {
        (IMM_F64_TAG as u64) | ((bits >> 7) << 7)
    }
}

pub(crate) fn emit_from_f64_inline_const(
    s: &mut wasm_encoder::InstructionSink<'_>,
    v: f64,
    dst_loc: u32,
) {
    s.i64_const(encode_f64_imm(v) as i64).local_set(dst_loc);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn register_runtime_functions(
    _target: WasmTarget,
    types: &mut TypeSection,
    functions: &mut FunctionSection,
    next_func_idx: &mut u32,
) -> (RtFuncIds, Vec<(u32, u8)>) {
    let mut add_ty = |params: &[ValType], results: &[ValType]| -> u32 {
        let idx = types.len();
        types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        idx
    };

    // Core Nessa ABI: (env_ptr: i32, argc: i32) -> i64
    let nessa_fn_type = add_ty(&[ValType::I32, ValType::I32], &[ValType::I64]);
    let ty_i32_to_i32 = add_ty(&[ValType::I32], &[ValType::I32]);
    let ty_4i32_to_i32 = add_ty(
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
    );
    let ty_2i32_to_i64 = add_ty(&[ValType::I32, ValType::I32], &[ValType::I64]);
    let ty_2i64_to_i64 = add_ty(&[ValType::I64, ValType::I64], &[ValType::I64]);
    let ty_2i64_to_i32 = add_ty(&[ValType::I64, ValType::I64], &[ValType::I32]);
    let ty_i64_to_i64 = add_ty(&[ValType::I64], &[ValType::I64]);
    let ty_i64_to_f64 = add_ty(&[ValType::I64], &[ValType::F64]);
    let ty_f64_to_i64 = add_ty(&[ValType::F64], &[ValType::I64]);
    let ty_write_str = add_ty(&[ValType::I32, ValType::I64, ValType::I32], &[]);
    let ty_cmp = add_ty(&[ValType::I64, ValType::I64, ValType::I32], &[ValType::I64]);
    let ty_i32_to_i64 = add_ty(&[ValType::I32], &[ValType::I64]);
    let ty_2i64_void = add_ty(&[ValType::I64, ValType::I64], &[]);
    let ty_3i64_void = add_ty(&[ValType::I64, ValType::I64, ValType::I64], &[]);
    let ty_i64_to_i32 = add_ty(&[ValType::I64], &[ValType::I32]);
    let ty_i64_i32_to_i64 = add_ty(&[ValType::I64, ValType::I32], &[ValType::I64]);
    let ty_i64_i32_to_i32 = add_ty(&[ValType::I64, ValType::I32], &[ValType::I32]);
    let ty_cast = add_ty(&[ValType::I64, ValType::I32, ValType::I32], &[ValType::I64]);

    let mut decls = Vec::new();
    let mut reg = |ty_idx: u32, tag: u8| -> u32 {
        let id = *next_func_idx;
        *next_func_idx += 1;
        functions.function(ty_idx);
        decls.push((ty_idx, tag));
        id
    };

    let ids = RtFuncIds {
        nessa_fn_type,
        alloc: reg(ty_i32_to_i32, 0),
        alloc_obj: reg(ty_4i32_to_i32, 1),
        alloc_str: reg(ty_2i32_to_i64, 2),
        str_concat: reg(ty_2i64_to_i64, 3),
        str_eq: reg(ty_2i64_to_i32, 4),
        val_eq: reg(ty_2i64_to_i32, 5),
        to_i64: reg(ty_i64_to_i64, 6),
        from_i64: reg(ty_i64_to_i64, 7),
        to_f64: reg(ty_i64_to_f64, 8),
        from_f64: reg(ty_f64_to_i64, 9),
        int_to_str: reg(ty_i64_to_i64, 10),
        f64_to_str: reg(ty_f64_to_i64, 11),
        to_string: reg(ty_i64_to_i64, 12),
        write_str: reg(ty_write_str, 13),
        add: reg(ty_2i64_to_i64, 14),
        sub: reg(ty_2i64_to_i64, 15),
        mul: reg(ty_2i64_to_i64, 16),
        div: reg(ty_2i64_to_i64, 17),
        rem: reg(ty_2i64_to_i64, 18),
        neg: reg(ty_i64_to_i64, 19),
        cmp: reg(ty_cmp, 20),
        list_new: reg(ty_i32_to_i64, 21),
        list_push: reg(ty_2i64_void, 22),
        list_pop: reg(ty_i64_to_i64, 23),
        list_get: reg(ty_2i64_to_i64, 24),
        list_set: reg(ty_3i64_void, 25),
        map_new: reg(ty_i32_to_i64, 26),
        map_set: reg(ty_3i64_void, 27),
        map_get: reg(ty_2i64_to_i64, 28),
        map_contains: reg(ty_2i64_to_i64, 29),
        map_remove: reg(ty_2i64_to_i64, 30),
        map_keys: reg(ty_i64_to_i64, 31),
        bytes_new: reg(ty_i32_to_i64, 32),
        bytes_from_str: reg(ty_i64_to_i64, 33),
        bytes_to_str: reg(ty_i64_to_i64, 34),
        bytes_get: reg(ty_2i64_to_i64, 35),
        bytes_set: reg(ty_3i64_void, 36),
        bytes_push: reg(ty_2i64_void, 37),
        reflected_type: reg(ty_i64_to_i32, 38),
        find_method: reg(ty_2i32_to_i64, 39),
        trait_proof: reg(ty_i64_i32_to_i64, 40),
        trait_vtable_func: reg(ty_i64_i32_to_i64, 41),
        type_check: reg(ty_i64_i32_to_i32, 42),
        type_cast: reg(ty_cast, 43),
        trap_unreachable: reg(nessa_fn_type, 44),
    };
    (ids, decls)
}

pub(crate) fn emit_runtime_bodies(
    codes: &mut CodeSection,
    rt: &RtFuncIds,
    info: &StaticLayoutInfo,
) {
    emit_alloc(codes);
    emit_alloc_obj(codes, rt);
    emit_alloc_str(codes, rt);
    emit_str_concat(codes, rt);
    emit_str_eq(codes);
    emit_val_eq(codes, rt);
    emit_to_i64(codes);
    emit_from_i64(codes, rt);
    emit_to_f64(codes);
    emit_from_f64(codes);
    emit_int_to_str(codes, rt);
    emit_f64_to_str(codes, rt);
    emit_to_string(codes, rt);
    emit_write_str(codes, info);
    emit_bin_arith(codes, rt, 0); // add
    emit_bin_arith(codes, rt, 1); // sub
    emit_bin_arith(codes, rt, 2); // mul
    emit_bin_arith(codes, rt, 3); // div
    emit_bin_arith(codes, rt, 4); // rem
    emit_neg(codes, rt);
    emit_cmp(codes, rt);
    emit_list_new(codes, rt, info);
    emit_list_push(codes, rt);
    emit_list_pop(codes);
    emit_list_get(codes, rt);
    emit_list_set(codes, rt);
    emit_map_new(codes, rt, info);
    emit_map_set(codes, rt);
    emit_map_get(codes, rt);
    emit_map_contains(codes, rt);
    emit_map_remove(codes, rt);
    emit_map_keys(codes, rt);
    emit_bytes_new(codes, rt, info);
    emit_bytes_from_str(codes, rt);
    emit_bytes_to_str(codes, rt);
    emit_bytes_get(codes, rt);
    emit_bytes_set(codes, rt);
    emit_bytes_push(codes, rt);
    emit_reflected_type(codes, info);
    emit_find_method(codes, info);
    emit_trait_proof(codes, rt, info);
    emit_trait_vtable_func(codes, info);
    emit_type_check(codes, rt, info);
    emit_type_cast(codes, rt, info);
    emit_trap_unreachable(codes);
}

fn emit_alloc(codes: &mut CodeSection) {
    // (param $bytes i32) -> (result i32)
    // locals: 1=$aligned i32, 2=$ptr i32, 3=$end i32, 4=$cur_bytes i32, 5=$need_pages i32
    let mut f = Function::new([(5, ValType::I32)]);
    let mut s = f.instructions();
    // aligned = (max(bytes, 8) + 7) & !7
    s.local_get(0)
        .i32_const(0)
        .i32_lt_s()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(0)
        .i32_const(8)
        .local_get(0)
        .i32_const(8)
        .i32_gt_u()
        .select()
        .i32_const(7)
        .i32_add()
        .i32_const(-8)
        .i32_and()
        .local_set(1)
        // ptr = (global.get $heap_ptr + 7) & !7
        .global_get(GLOBAL_HEAP_PTR)
        .i32_const(7)
        .i32_add()
        .i32_const(-8)
        .i32_and()
        .local_set(2)
        // end = ptr + aligned
        .local_get(2)
        .local_get(1)
        .i32_add()
        .local_set(3)
        // overflow check
        .local_get(3)
        .local_get(2)
        .i32_lt_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        // cur_bytes = memory.size * 65536
        .memory_size(0)
        .i32_const(16)
        .i32_shl()
        .local_set(4)
        .local_get(3)
        .local_get(4)
        .i32_gt_u()
        .if_(BlockType::Empty)
        .local_get(3)
        .local_get(4)
        .i32_sub()
        .i32_const(65535)
        .i32_add()
        .i32_const(16)
        .i32_shr_u()
        .local_set(5)
        .local_get(5)
        .memory_grow(0)
        .i32_const(-1)
        .i32_eq()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .end()
        .local_get(3)
        .global_set(GLOBAL_HEAP_PTR)
        .local_get(2)
        .end();
    codes.function(&f);
}

fn emit_alloc_obj(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=type_idx i32, 1=role i32, 2=words_or_len i32, 3=aux i32) -> i32 (payload_ptr)
    // locals: 4=$pay_bytes i32, 5=$raw i32, 6=$pay i32
    let mut f = Function::new([(3, ValType::I32)]);
    let mut s = f.instructions();
    // if role == ROLE_STRING_UTF8: pay_bytes = words_or_len; else pay_bytes = words_or_len * 8
    s.local_get(1)
        .i32_const(ROLE_STRING_UTF8)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(2)
        .else_()
        .local_get(2)
        .i32_const(3)
        .i32_shl()
        .end()
        .local_set(4)
        // raw = alloc(16 + pay_bytes)
        .local_get(4)
        .i32_const(16)
        .i32_add()
        .call(rt.alloc)
        .local_set(5)
        // write header at raw + 0..16
        .local_get(5)
        .local_get(0)
        .i32_store(mem(0, 2))
        .local_get(5)
        .local_get(1)
        .i32_store(mem(4, 2))
        .local_get(5)
        .local_get(2)
        .i32_store(mem(8, 2))
        .local_get(5)
        .local_get(3)
        .i32_store(mem(12, 2))
        // payload_ptr = raw + 16 (guaranteed 8-byte aligned!)
        .local_get(5)
        .i32_const(16)
        .i32_add()
        .local_tee(6)
        .end();
    codes.function(&f);
}

fn emit_alloc_str(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=src_ptr i32, 1=len i32) -> i64 (TaggedValue)
    // local 2=$dst i32
    let mut f = Function::new([(1, ValType::I32)]);
    let mut s = f.instructions();
    s.i32_const(TY_STR as i32)
        .i32_const(ROLE_STRING_UTF8)
        .local_get(1)
        .i32_const(0)
        .call(rt.alloc_obj)
        .local_set(2)
        .local_get(1)
        .i32_const(0)
        .i32_gt_s()
        .if_(BlockType::Empty)
        .local_get(2)
        .local_get(0)
        .local_get(1)
        .memory_copy(0, 0)
        .end()
        .local_get(2)
        .i64_extend_i32_u()
        .end();
    codes.function(&f);
}

fn emit_str_concat(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=a i64, 1=b i64) -> i64
    // locals: 2=pa i32, 3=pb i32, 4=la i32, 5=lb i32, 6=dst i32
    let mut f = Function::new([(5, ValType::I32)]);
    let mut s = f.instructions();
    // Ensure both are heap strings (`(v & 7) == 0 && v != 0`)
    s.local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .i32_eqz()
        .local_get(1)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .i32_eqz()
        .i32_or()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_set(4)
        .local_get(1)
        .i32_wrap_i64()
        .local_tee(3)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_set(5)
        .i32_const(TY_STR as i32)
        .i32_const(ROLE_STRING_UTF8)
        .local_get(4)
        .local_get(5)
        .i32_add()
        .i32_const(0)
        .call(rt.alloc_obj)
        .local_set(6)
        .local_get(4)
        .i32_const(0)
        .i32_gt_s()
        .if_(BlockType::Empty)
        .local_get(6)
        .local_get(2)
        .local_get(4)
        .memory_copy(0, 0)
        .end()
        .local_get(5)
        .i32_const(0)
        .i32_gt_s()
        .if_(BlockType::Empty)
        .local_get(6)
        .local_get(4)
        .i32_add()
        .local_get(3)
        .local_get(5)
        .memory_copy(0, 0)
        .end()
        .local_get(6)
        .i64_extend_i32_u()
        .end();
    codes.function(&f);
}

fn emit_str_eq(codes: &mut CodeSection) {
    // (param 0=a i64, 1=b i64) -> i32
    // locals: 2=pa i32, 3=pb i32, 4=len i32, 5=i i32
    let mut f = Function::new([(4, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .local_get(1)
        .i64_eq()
        .if_(BlockType::Empty)
        .i32_const(1)
        .return_()
        .end()
        .local_get(0)
        .i32_wrap_i64()
        .local_set(2)
        .local_get(1)
        .i32_wrap_i64()
        .local_set(3)
        .local_get(2)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_tee(4)
        .local_get(3)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_ne()
        .if_(BlockType::Empty)
        .i32_const(0)
        .return_()
        .end()
        .i32_const(0)
        .local_set(5)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(5)
        .local_get(4)
        .i32_ge_u()
        .br_if(1)
        .local_get(2)
        .local_get(5)
        .i32_add()
        .i32_load8_u(mem(0, 0))
        .local_get(3)
        .local_get(5)
        .i32_add()
        .i32_load8_u(mem(0, 0))
        .i32_ne()
        .if_(BlockType::Empty)
        .i32_const(0)
        .return_()
        .end()
        .local_get(5)
        .i32_const(1)
        .i32_add()
        .local_set(5)
        .br(0)
        .end()
        .end()
        .i32_const(1)
        .end();
    codes.function(&f);
}

fn emit_val_eq(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=a i64, 1=b i64) -> i32
    // locals: 2=fa f64, 3=fb f64, 4=pa i32, 5=pb i32
    let mut f = Function::new([(2, ValType::F64), (2, ValType::I32)]);
    let mut s = f.instructions();
    // Check if either is float: ((v & 6) != 0 || (v & 0x7f) == IMM_F64_TAG)
    let is_f64 = |s: &mut wasm_encoder::InstructionSink<'_>, loc: u32| {
        s.local_get(loc)
            .i64_const(ROT_F64_TAG_MASK)
            .i64_and()
            .i64_const(0)
            .i64_ne()
            .local_get(loc)
            .i64_const(IMM_LOW7_MASK)
            .i64_and()
            .i64_const(IMM_F64_TAG)
            .i64_eq()
            .i32_or();
    };
    is_f64(&mut s, 0);
    is_f64(&mut s, 1);
    s.i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .local_get(1)
        .call(rt.to_f64)
        .f64_eq()
        .return_()
        .end();
    // Both heap?
    s.local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(0)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .local_get(1)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(1)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i32_wrap_i64()
        .local_tee(4)
        .i32_const(12)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(ROLE_STRING_UTF8)
        .i32_eq()
        .local_get(1)
        .i32_wrap_i64()
        .local_tee(5)
        .i32_const(12)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(ROLE_STRING_UTF8)
        .i32_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .local_get(1)
        .call(rt.str_eq)
        .return_()
        .end()
        .end();
    // Exact bit match for immediates (except NaN which was handled above)
    s.local_get(0).local_get(1).i64_eq().end();
    codes.function(&f);
}

fn emit_to_i64(codes: &mut CodeSection) {
    // (param 0=v i64) -> i64
    // local 1=low7 i64, 2=data i64, 3=kind i64, 4=ptr i32
    let mut f = Function::new([(3, ValType::I64), (1, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .local_set(1)
        // IMM_I_SMALL or IMM_ISIZE -> arithmetic right shift by 7
        .local_get(1)
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .local_get(1)
        .i64_const(IMM_ISIZE_TAG)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_s()
        .return_()
        .end()
        // IMM_U_SMALL or IMM_USIZE or IMM_CHAR -> unsigned right shift by 7
        .local_get(1)
        .i64_const(IMM_U_SMALL_TAG)
        .i64_eq()
        .local_get(1)
        .i64_const(IMM_USIZE_TAG)
        .i64_eq()
        .i32_or()
        .local_get(1)
        .i64_const(IMM_CHAR_TAG)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .return_()
        .end()
        // IMM_BOOL
        .local_get(1)
        .i64_const(IMM_BOOL_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .return_()
        .end()
        // IMM_NARROW
        .local_get(1)
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .local_tee(2)
        .i64_const(32)
        .i64_shr_u()
        .i64_const(0xf)
        .i64_and()
        .local_set(3)
        // kind <= 2 (U8, U16, U32): zero-extended 32-bit
        .local_get(3)
        .i64_const(2)
        .i64_le_u()
        .if_(BlockType::Empty)
        .local_get(2)
        .i64_const(0xffff_ffff)
        .i64_and()
        .return_()
        .end()
        // kind 3..=5 (I8, I16, I32): sign-extend 32-bit
        .local_get(3)
        .i64_const(5)
        .i64_le_u()
        .if_(BlockType::Empty)
        .local_get(2)
        .i32_wrap_i64()
        .i64_extend_i32_s()
        .return_()
        .end()
        // kind == 6 (F32): trunc to i64
        .local_get(2)
        .i32_wrap_i64()
        .f32_reinterpret_i32()
        .i64_trunc_f32_s()
        .return_()
        .end()
        // Rotated / IMM f64 -> trunc to i64
        .local_get(0)
        .i64_const(ROT_F64_TAG_MASK)
        .i64_and()
        .i64_const(0)
        .i64_ne()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(4)
        .i64_rotr()
        .f64_reinterpret_i64()
        .i64_trunc_f64_s()
        .return_()
        .end()
        .local_get(1)
        .i64_const(IMM_F64_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(!IMM_LOW7_MASK)
        .i64_and()
        .f64_reinterpret_i64()
        .i64_trunc_f64_s()
        .return_()
        .end()
        // Boxed 64-bit integer on heap
        .local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(0)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i32_wrap_i64()
        .local_tee(4)
        .i32_const(12)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(ROLE_BOXED_SCALAR)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(4)
        .i64_load(mem(0, 3))
        .return_()
        .end()
        .end()
        .unreachable()
        .end();
    codes.function(&f);
}

fn emit_from_i64(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=x i64) -> i64
    // local 1=ptr i32
    let mut f = Function::new([(1, ValType::I32)]);
    let mut s = f.instructions();
    // Check -(1 << 56) <= x < (1 << 56): i.e. ((x << 7) >> 7) == x
    s.local_get(0)
        .i64_const(7)
        .i64_shl()
        .i64_const(7)
        .i64_shr_s()
        .local_get(0)
        .i64_eq()
        .if_(BlockType::Result(ValType::I64))
        .local_get(0)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_I_SMALL_TAG)
        .i64_or()
        .else_()
        .i32_const(TY_I64 as i32)
        .i32_const(ROLE_BOXED_SCALAR)
        .i32_const(1)
        .i32_const(0)
        .call(rt.alloc_obj)
        .local_tee(1)
        .local_get(0)
        .i64_store(mem(0, 3))
        .local_get(1)
        .i64_extend_i32_u()
        .end()
        .end();
    codes.function(&f);
}

fn emit_to_f64(codes: &mut CodeSection) {
    // (param 0=v i64) -> f64
    // local 1=low7 i64, 2=data i64, 3=kind i64
    let mut f = Function::new([(3, ValType::I64)]);
    let mut s = f.instructions();
    // Fast path: rotated f64
    s.local_get(0)
        .i64_const(ROT_F64_TAG_MASK)
        .i64_and()
        .i64_const(0)
        .i64_ne()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(4)
        .i64_rotr()
        .f64_reinterpret_i64()
        .return_()
        .end()
        .local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .local_set(1)
        // IMM_F64 (+0.0, -0.0, tiny dyadic)
        .local_get(1)
        .i64_const(IMM_F64_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(!IMM_LOW7_MASK)
        .i64_and()
        .f64_reinterpret_i64()
        .return_()
        .end()
        // IMM_I_SMALL / IMM_ISIZE
        .local_get(1)
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .local_get(1)
        .i64_const(IMM_ISIZE_TAG)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_s()
        .f64_convert_i64_s()
        .return_()
        .end()
        // IMM_U_SMALL / IMM_USIZE
        .local_get(1)
        .i64_const(IMM_U_SMALL_TAG)
        .i64_eq()
        .local_get(1)
        .i64_const(IMM_USIZE_TAG)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .f64_convert_i64_u()
        .return_()
        .end()
        // IMM_NARROW
        .local_get(1)
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .local_tee(2)
        .i64_const(32)
        .i64_shr_u()
        .i64_const(0xf)
        .i64_and()
        .local_set(3)
        .local_get(3)
        .i64_const(NARROW_F32)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(2)
        .i32_wrap_i64()
        .f32_reinterpret_i32()
        .f64_promote_f32()
        .return_()
        .end()
        .local_get(3)
        .i64_const(2)
        .i64_le_u()
        .if_(BlockType::Empty)
        .local_get(2)
        .i32_wrap_i64()
        .f64_convert_i32_u()
        .return_()
        .end()
        .local_get(2)
        .i32_wrap_i64()
        .f64_convert_i32_s()
        .return_()
        .end()
        // Boxed f64 on heap
        .local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(0)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i32_wrap_i64()
        .f64_load(mem(0, 3))
        .return_()
        .end()
        .unreachable()
        .end();
    codes.function(&f);
}

fn emit_from_f64(codes: &mut CodeSection) {
    // (param 0=x f64) -> i64
    // local 1=bits i64, 2=rot i64
    let mut f = Function::new([(2, ValType::I64)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i64_reinterpret_f64()
        .local_tee(1)
        .i64_const(4)
        .i64_rotl()
        .local_tee(2)
        .i64_const(ROT_F64_TAG_MASK)
        .i64_and()
        .i64_const(0)
        .i64_ne()
        .if_(BlockType::Result(ValType::I64))
        .local_get(2)
        .else_()
        .local_get(1)
        .i64_const(!IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_F64_TAG)
        .i64_or()
        .end()
        .end();
    codes.function(&f);
}

fn emit_int_to_str(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=n i64) -> i64 (HeapString)
    // Formats signed 64-bit integer backwards into FMT_SCRATCH_ADDR..+64
    // locals: 1=mag i64, 2=neg i32, 3=pos i32
    let mut f = Function::new([(1, ValType::I64), (2, ValType::I32)]);
    let mut s = f.instructions();
    s.i32_const(FMT_SCRATCH_ADDR + 64)
        .local_set(3)
        .local_get(0)
        .i64_eqz()
        .if_(BlockType::Empty)
        .local_get(3)
        .i32_const(1)
        .i32_sub()
        .local_tee(3)
        .i32_const(b'0' as i32)
        .i32_store8(mem(0, 0))
        .local_get(3)
        .i32_const(1)
        .call(rt.alloc_str)
        .return_()
        .end()
        .local_get(0)
        .i64_const(0)
        .i64_lt_s()
        .if_(BlockType::Empty)
        .i32_const(1)
        .local_set(2)
        .i64_const(0)
        .local_get(0)
        .i64_sub()
        .local_set(1)
        .else_()
        .i32_const(0)
        .local_set(2)
        .local_get(0)
        .local_set(1)
        .end()
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(1)
        .i64_eqz()
        .br_if(1)
        .local_get(3)
        .i32_const(1)
        .i32_sub()
        .local_tee(3)
        .local_get(1)
        .i64_const(10)
        .i64_rem_u()
        .i32_wrap_i64()
        .i32_const(b'0' as i32)
        .i32_add()
        .i32_store8(mem(0, 0))
        .local_get(1)
        .i64_const(10)
        .i64_div_u()
        .local_set(1)
        .br(0)
        .end()
        .end()
        .local_get(2)
        .if_(BlockType::Empty)
        .local_get(3)
        .i32_const(1)
        .i32_sub()
        .local_tee(3)
        .i32_const(b'-' as i32)
        .i32_store8(mem(0, 0))
        .end()
        .local_get(3)
        .i32_const(FMT_SCRATCH_ADDR + 64)
        .local_get(3)
        .i32_sub()
        .call(rt.alloc_str)
        .end();
    codes.function(&f);
}

fn emit_f64_to_str(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=x f64) -> i64 (HeapString)
    // Handles NaN, Inf, -Inf, exact integer-valued floats, and up to 6 fractional digits (trimmed)
    // locals: 1=abs f64, 2=int_part i64, 3=frac_scaled i64, 4=s_head i64, 5=pos i32, 6=cnt i32, 7=neg i32
    let mut f = Function::new([(1, ValType::F64), (3, ValType::I64), (3, ValType::I32)]);
    let mut s = f.instructions();
    // NaN check: x != x
    s.local_get(0)
        .local_get(0)
        .f64_ne()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(b'N' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .i32_const(b'a' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 2)
        .i32_const(b'N' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(3)
        .call(rt.alloc_str)
        .return_()
        .end();
    // +Inf
    s.local_get(0)
        .f64_const(wasm_encoder::Ieee64::new(f64::INFINITY.to_bits()))
        .f64_eq()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(b'i' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .i32_const(b'n' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 2)
        .i32_const(b'f' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(3)
        .call(rt.alloc_str)
        .return_()
        .end();
    // -Inf
    s.local_get(0)
        .f64_const(wasm_encoder::Ieee64::new(f64::NEG_INFINITY.to_bits()))
        .f64_eq()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(b'-' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .i32_const(b'i' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 2)
        .i32_const(b'n' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 3)
        .i32_const(b'f' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(4)
        .call(rt.alloc_str)
        .return_()
        .end();

    // Sign & abs
    s.local_get(0)
        .f64_const(wasm_encoder::Ieee64::new(0.0_f64.to_bits()))
        .f64_lt()
        .if_(BlockType::Empty)
        .i32_const(1)
        .local_set(7)
        .local_get(0)
        .f64_neg()
        .local_set(1)
        .else_()
        .i32_const(0)
        .local_set(7)
        .local_get(0)
        .local_set(1)
        .end();

    // int_part = trunc(abs), frac_scaled = nearest((abs - floor(abs)) * 1_000_000)
    s.local_get(1)
        .f64_floor()
        .i64_trunc_sat_f64_u()
        .local_set(2)
        .local_get(1)
        .local_get(1)
        .f64_floor()
        .f64_sub()
        .f64_const(wasm_encoder::Ieee64::new(1_000_000.0_f64.to_bits()))
        .f64_mul()
        .f64_nearest()
        .i64_trunc_sat_f64_u()
        .local_set(3);
    // Carry if rounding reached 1_000_000
    s.local_get(3)
        .i64_const(1_000_000)
        .i64_ge_u()
        .if_(BlockType::Empty)
        .local_get(2)
        .i64_const(1)
        .i64_add()
        .local_set(2)
        .i64_const(0)
        .local_set(3)
        .end();

    // Format integer head (signed)
    s.local_get(7)
        .if_(BlockType::Result(ValType::I64))
        .i64_const(0)
        .local_get(2)
        .i64_sub()
        .call(rt.int_to_str)
        .else_()
        .local_get(2)
        .call(rt.int_to_str)
        .end()
        .local_set(4);
    // Special case -0.x when int_part == 0 and neg == 1
    s.local_get(7)
        .local_get(2)
        .i64_eqz()
        .i32_and()
        .local_get(3)
        .i64_const(0)
        .i64_gt_u()
        .i32_and()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(b'-' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .i32_const(b'0' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(2)
        .call(rt.alloc_str)
        .local_set(4)
        .end();

    // If frac_scaled == 0: Nessa formats integer-valued floats like `3` or `3.0`?
    // Wait: in Nessa `to_string(3.0)` uses Rust Display on f64 which prints `"3"` when fractional is 0!
    // Let's return `s_head` directly when `frac_scaled == 0`!
    s.local_get(3)
        .i64_eqz()
        .if_(BlockType::Empty)
        .local_get(4)
        .return_()
        .end();

    // Format `.dddddd` into FMT_SCRATCH_ADDR + 72..+79 and trim trailing '0's
    s.i32_const(FMT_SCRATCH_ADDR + 72)
        .i32_const(b'.' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(6)
        .local_set(6)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(6)
        .i32_eqz()
        .br_if(1)
        .i32_const(FMT_SCRATCH_ADDR + 72)
        .local_get(6)
        .i32_add()
        .local_get(3)
        .i64_const(10)
        .i64_rem_u()
        .i32_wrap_i64()
        .i32_const(b'0' as i32)
        .i32_add()
        .i32_store8(mem(0, 0))
        .local_get(3)
        .i64_const(10)
        .i64_div_u()
        .local_set(3)
        .local_get(6)
        .i32_const(1)
        .i32_sub()
        .local_set(6)
        .br(0)
        .end()
        .end();

    // pos = 7 (1 dot + 6 digits), trim trailing '0'
    s.i32_const(7)
        .local_set(5)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(5)
        .i32_const(2)
        .i32_le_s()
        .br_if(1)
        .i32_const(FMT_SCRATCH_ADDR + 72)
        .local_get(5)
        .i32_add()
        .i32_const(1)
        .i32_sub()
        .i32_load8_u(mem(0, 0))
        .i32_const(b'0' as i32)
        .i32_ne()
        .br_if(1)
        .local_get(5)
        .i32_const(1)
        .i32_sub()
        .local_set(5)
        .br(0)
        .end()
        .end();

    s.local_get(4)
        .i32_const(FMT_SCRATCH_ADDR + 72)
        .local_get(5)
        .call(rt.alloc_str)
        .call(rt.str_concat)
        .end();
    codes.function(&f);
}

fn emit_to_string(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=v i64) -> i64 (HeapString)
    // locals: 1=low7 i64, 2=cp i32, 3=ptr i32
    let mut f = Function::new([(1, ValType::I64), (2, ValType::I32)]);
    let mut s = f.instructions();
    // Heap value?
    s.local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(0)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i32_wrap_i64()
        .local_tee(3)
        .i32_const(12)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(ROLE_STRING_UTF8)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .return_()
        .end()
        .local_get(3)
        .i32_const(12)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(ROLE_BOXED_SCALAR)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(3)
        .i64_load(mem(0, 3))
        .call(rt.int_to_str)
        .return_()
        .end()
        // Fallback "<object>"
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(b'<' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .i32_const(b'o' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 2)
        .i32_const(b'b' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 3)
        .i32_const(b'j' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 4)
        .i32_const(b'>' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(5)
        .call(rt.alloc_str)
        .return_()
        .end();

    // Rotated f64
    s.local_get(0)
        .i64_const(ROT_F64_TAG_MASK)
        .i64_and()
        .i64_const(0)
        .i64_ne()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .call(rt.f64_to_str)
        .return_()
        .end();

    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .local_set(1);

    // IMM_F64
    s.local_get(1)
        .i64_const(IMM_F64_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .call(rt.f64_to_str)
        .return_()
        .end();

    // Bool
    s.local_get(0)
        .i64_const(VAL_TRUE)
        .i64_eq()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(0x6575_7274) // "true" little-endian
        .i32_store(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(4)
        .call(rt.alloc_str)
        .return_()
        .end();
    s.local_get(0)
        .i64_const(VAL_FALSE)
        .i64_eq()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(0x736c_6166) // "fals"
        .i32_store(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 4)
        .i32_const(b'e' as i32)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(5)
        .call(rt.alloc_str)
        .return_()
        .end();

    // Null
    s.local_get(0)
        .i64_const(VAL_NULL)
        .i64_eq()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(0x6c6c_756e) // "null"
        .i32_store(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(4)
        .call(rt.alloc_str)
        .return_()
        .end();

    // Unit "()"
    s.local_get(0)
        .i64_const(VAL_UNIT)
        .i64_eq()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(0x2928) // "()"
        .i32_store16(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(2)
        .call(rt.alloc_str)
        .return_()
        .end();

    // Char (encode UTF-8 1..4 bytes)
    s.local_get(1)
        .i64_const(IMM_CHAR_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .i32_wrap_i64()
        .local_tee(2)
        .i32_const(0x80)
        .i32_lt_u()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .local_get(2)
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(1)
        .call(rt.alloc_str)
        .return_()
        .end()
        .local_get(2)
        .i32_const(0x800)
        .i32_lt_u()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .local_get(2)
        .i32_const(6)
        .i32_shr_u()
        .i32_const(0xC0)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .local_get(2)
        .i32_const(0x3F)
        .i32_and()
        .i32_const(0x80)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(2)
        .call(rt.alloc_str)
        .return_()
        .end()
        .local_get(2)
        .i32_const(0x10000)
        .i32_lt_u()
        .if_(BlockType::Empty)
        .i32_const(FMT_SCRATCH_ADDR)
        .local_get(2)
        .i32_const(12)
        .i32_shr_u()
        .i32_const(0xE0)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .local_get(2)
        .i32_const(6)
        .i32_shr_u()
        .i32_const(0x3F)
        .i32_and()
        .i32_const(0x80)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 2)
        .local_get(2)
        .i32_const(0x3F)
        .i32_and()
        .i32_const(0x80)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(3)
        .call(rt.alloc_str)
        .return_()
        .end()
        .i32_const(FMT_SCRATCH_ADDR)
        .local_get(2)
        .i32_const(18)
        .i32_shr_u()
        .i32_const(0xF0)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 1)
        .local_get(2)
        .i32_const(12)
        .i32_shr_u()
        .i32_const(0x3F)
        .i32_and()
        .i32_const(0x80)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 2)
        .local_get(2)
        .i32_const(6)
        .i32_shr_u()
        .i32_const(0x3F)
        .i32_and()
        .i32_const(0x80)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR + 3)
        .local_get(2)
        .i32_const(0x3F)
        .i32_and()
        .i32_const(0x80)
        .i32_or()
        .i32_store8(mem(0, 0))
        .i32_const(FMT_SCRATCH_ADDR)
        .i32_const(4)
        .call(rt.alloc_str)
        .return_()
        .end();

    // Narrow F32 check before integer fallback
    s.local_get(1)
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(39)
        .i64_shr_u()
        .i64_const(0xf)
        .i64_and()
        .i64_const(NARROW_F32)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .call(rt.f64_to_str)
        .return_()
        .end()
        .end();

    // All remaining integer immediates
    s.local_get(0).call(rt.to_i64).call(rt.int_to_str).end();
    codes.function(&f);
}

fn emit_write_str(codes: &mut CodeSection, info: &StaticLayoutInfo) {
    // (param 0=fd i32, 1=str_val i64, 2=newline i32)
    // local 3=ptr i32, 4=len i32
    let mut f = Function::new([(2, ValType::I32)]);
    let mut s = f.instructions();
    if let Some(fd_write) = info.wasi_fd_write {
        s.local_get(1)
            .i32_wrap_i64()
            .local_tee(3)
            .i32_const(8)
            .i32_sub()
            .i32_load(mem(0, 2))
            .local_set(4)
            .local_get(4)
            .i32_const(0)
            .i32_gt_s()
            .if_(BlockType::Empty)
            .i32_const(WASI_IOVEC_ADDR as i32)
            .local_get(3)
            .i32_store(mem(0, 2))
            .i32_const(WASI_IOVEC_ADDR as i32)
            .local_get(4)
            .i32_store(mem(4, 2))
            .local_get(0)
            .i32_const(WASI_IOVEC_ADDR as i32)
            .i32_const(1)
            .i32_const(WASI_NWRITTEN_ADDR as i32)
            .call(fd_write)
            .drop()
            .end()
            .local_get(2)
            .if_(BlockType::Empty)
            .i32_const(NEWLINE_BYTE_ADDR)
            .i32_const(b'\n' as i32)
            .i32_store8(mem(0, 0))
            .i32_const(WASI_IOVEC_ADDR as i32)
            .i32_const(NEWLINE_BYTE_ADDR)
            .i32_store(mem(0, 2))
            .i32_const(WASI_IOVEC_ADDR as i32)
            .i32_const(1)
            .i32_store(mem(4, 2))
            .local_get(0)
            .i32_const(WASI_IOVEC_ADDR as i32)
            .i32_const(1)
            .i32_const(WASI_NWRITTEN_ADDR as i32)
            .call(fd_write)
            .drop()
            .end();
    }
    s.end();
    codes.function(&f);
}

fn emit_bin_arith(codes: &mut CodeSection, rt: &RtFuncIds, op: u8) {
    // op: 0=add, 1=sub, 2=mul, 3=div, 4=rem
    // params: 0=a (i64), 1=b (i64) -> i64; locals: 2..=8 (i64), 9..=10 (f64)
    let mut f = Function::new([(7, ValType::I64), (2, ValType::F64)]);
    let mut s = f.instructions();
    s.local_get(0)
        .local_get(1)
        .i64_or()
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .local_get(0)
        .local_get(1)
        .i64_and()
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_s()
        .local_set(6)
        .local_get(1)
        .i64_const(7)
        .i64_shr_s()
        .local_set(7);
    match op {
        0 => {
            s.local_get(6).local_get(7).i64_add().local_set(8);
        }
        1 => {
            s.local_get(6).local_get(7).i64_sub().local_set(8);
        }
        2 => {
            s.local_get(6).local_get(7).i64_mul().local_set(8);
        }
        3 => {
            s.local_get(6).local_get(7).i64_div_s().local_set(8);
        }
        _ => {
            s.local_get(6).local_get(7).i64_rem_s().local_set(8);
        }
    }
    s.local_get(8).call(rt.from_i64).return_().end();

    if op == 0 {
        s.local_get(0)
            .i64_const(TAG_MASK)
            .i64_and()
            .i64_eqz()
            .local_get(0)
            .i64_const(0)
            .i64_ne()
            .i32_and()
            .local_get(1)
            .i64_const(TAG_MASK)
            .i64_and()
            .i64_eqz()
            .local_get(1)
            .i64_const(0)
            .i64_ne()
            .i32_and()
            .i32_and()
            .if_(BlockType::Empty)
            .local_get(0)
            .i32_wrap_i64()
            .i32_const(12)
            .i32_sub()
            .i32_load(mem(0, 2))
            .i32_const(ROLE_STRING_UTF8)
            .i32_eq()
            .if_(BlockType::Empty)
            .local_get(0)
            .local_get(1)
            .call(rt.str_concat)
            .return_()
            .end()
            .end();
    }

    let check_float = |s: &mut wasm_encoder::InstructionSink<'_>, loc: u32| {
        s.local_get(loc)
            .i64_const(ROT_F64_TAG_MASK)
            .i64_and()
            .i64_const(0)
            .i64_ne()
            .local_get(loc)
            .i64_const(IMM_LOW7_MASK)
            .i64_and()
            .i64_const(IMM_F64_TAG)
            .i64_eq()
            .i32_or()
            .local_get(loc)
            .i64_const(IMM_LOW7_MASK)
            .i64_and()
            .i64_const(IMM_NARROW_TAG)
            .i64_eq()
            .local_get(loc)
            .i64_const(39)
            .i64_shr_u()
            .i64_const(0xf)
            .i64_and()
            .i64_const(NARROW_F32)
            .i64_eq()
            .i32_and()
            .i32_or();
    };
    check_float(&mut s, 0);
    check_float(&mut s, 1);
    s.i32_or()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .local_set(9)
        .local_get(1)
        .call(rt.to_f64)
        .local_set(10);
    match op {
        0 => {
            s.local_get(9).local_get(10).f64_add();
        }
        1 => {
            s.local_get(9).local_get(10).f64_sub();
        }
        2 => {
            s.local_get(9).local_get(10).f64_mul();
        }
        3 => {
            s.local_get(9).local_get(10).f64_div();
        }
        _ => {
            // a - trunc(a / b) * b
            s.local_get(9)
                .local_get(9)
                .local_get(10)
                .f64_div()
                .f64_trunc()
                .local_get(10)
                .f64_mul()
                .f64_sub();
        }
    }
    // Preserve F32 tag if first operand was F32
    s.local_set(9)
        .local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .local_get(0)
        .i64_const(39)
        .i64_shr_u()
        .i64_const(0xf)
        .i64_and()
        .i64_const(NARROW_F32)
        .i64_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(9)
        .f32_demote_f64()
        .i32_reinterpret_f32()
        .i64_extend_i32_u()
        .i64_const(NARROW_F32 << 32)
        .i64_or()
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_NARROW_TAG)
        .i64_or()
        .return_()
        .end()
        .local_get(9)
        .call(rt.from_f64)
        .return_()
        .end();

    // General integer fallback (preserves Usize, Isize, U64, I32, U32, etc. from lhs tag!)
    s.local_get(0)
        .call(rt.to_i64)
        .local_set(6)
        .local_get(1)
        .call(rt.to_i64)
        .local_set(7)
        .local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .local_set(2);
    // Unsigned vs signed division/rem
    let is_unsigned_lhs = |s: &mut wasm_encoder::InstructionSink<'_>| {
        s.local_get(2)
            .i64_const(IMM_U_SMALL_TAG)
            .i64_eq()
            .local_get(2)
            .i64_const(IMM_USIZE_TAG)
            .i64_eq()
            .i32_or()
            .local_get(2)
            .i64_const(IMM_NARROW_TAG)
            .i64_eq()
            .local_get(0)
            .i64_const(39)
            .i64_shr_u()
            .i64_const(0xf)
            .i64_and()
            .i64_const(2)
            .i64_le_u()
            .i32_and()
            .i32_or();
    };
    match op {
        0 => {
            s.local_get(6).local_get(7).i64_add().local_set(8);
        }
        1 => {
            s.local_get(6).local_get(7).i64_sub().local_set(8);
        }
        2 => {
            s.local_get(6).local_get(7).i64_mul().local_set(8);
        }
        3 => {
            is_unsigned_lhs(&mut s);
            s.if_(BlockType::Result(ValType::I64))
                .local_get(6)
                .local_get(7)
                .i64_div_u()
                .else_()
                .local_get(6)
                .local_get(7)
                .i64_div_s()
                .end()
                .local_set(8);
        }
        _ => {
            is_unsigned_lhs(&mut s);
            s.if_(BlockType::Result(ValType::I64))
                .local_get(6)
                .local_get(7)
                .i64_rem_u()
                .else_()
                .local_get(6)
                .local_get(7)
                .i64_rem_s()
                .end()
                .local_set(8);
        }
    }
    // Re-tag according to lhs tag
    s.local_get(2)
        .i64_const(IMM_USIZE_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(8)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_USIZE_TAG)
        .i64_or()
        .return_()
        .end()
        .local_get(2)
        .i64_const(IMM_ISIZE_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(8)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_ISIZE_TAG)
        .i64_or()
        .return_()
        .end()
        .local_get(2)
        .i64_const(IMM_U_SMALL_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(8)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_U_SMALL_TAG)
        .i64_or()
        .return_()
        .end()
        .local_get(2)
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(0x0000_0780_0000_0000) // preserve narrow kind bits [42:39]
        .i64_and()
        .local_get(8)
        .i64_const(0xffff_ffff)
        .i64_and()
        .i64_const(7)
        .i64_shl()
        .i64_or()
        .i64_const(IMM_NARROW_TAG)
        .i64_or()
        .return_()
        .end()
        .local_get(8)
        .call(rt.from_i64)
        .end();
    codes.function(&f);
}

fn emit_neg(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=a i64) -> i64
    let mut f = Function::new([(1, ValType::F64)]);
    let mut s = f.instructions();
    // IMM_I_SMALL fast path
    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .i64_const(0)
        .local_get(0)
        .i64_const(7)
        .i64_shr_s()
        .i64_sub()
        .call(rt.from_i64)
        .return_()
        .end();
    // Float?
    s.local_get(0)
        .i64_const(ROT_F64_TAG_MASK)
        .i64_and()
        .i64_const(0)
        .i64_ne()
        .local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_F64_TAG)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .f64_neg()
        .call(rt.from_f64)
        .return_()
        .end();
    // F32 narrow?
    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .local_get(0)
        .i64_const(39)
        .i64_shr_u()
        .i64_const(0xf)
        .i64_and()
        .i64_const(NARROW_F32)
        .i64_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .i32_wrap_i64()
        .f32_reinterpret_i32()
        .f32_neg()
        .i32_reinterpret_f32()
        .i64_extend_i32_u()
        .i64_const(NARROW_F32 << 32)
        .i64_or()
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_NARROW_TAG)
        .i64_or()
        .return_()
        .end();
    // Narrow I32/I16/I8
    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(0x0000_0780_0000_0000)
        .i64_and()
        .i64_const(0)
        .local_get(0)
        .call(rt.to_i64)
        .i64_sub()
        .i64_const(0xffff_ffff)
        .i64_and()
        .i64_const(7)
        .i64_shl()
        .i64_or()
        .i64_const(IMM_NARROW_TAG)
        .i64_or()
        .return_()
        .end();
    s.i64_const(0)
        .local_get(0)
        .call(rt.to_i64)
        .i64_sub()
        .call(rt.from_i64)
        .end();
    codes.function(&f);
}

fn emit_cmp(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=a i64, 1=b i64, 2=op i32) -> i64 (VAL_TRUE or VAL_FALSE)
    // op: 0=Eq, 1=Ne, 2=Lt, 3=Le, 4=Gt, 5=Ge
    // locals: 3=res i32, 4=fa f64, 5=fb f64, 6=ia i64, 7=ib i64, 8=pa i32, 9=pb i32, 10=len i32, 11=idx i32
    let mut f = Function::new([(5, ValType::I32), (2, ValType::F64), (2, ValType::I64)]);
    // locals: 3,4,5,6,7 = i32; 8,9 = f64; 10,11 = i64
    let mut s = f.instructions();
    // Eq / Ne delegate to val_eq
    s.local_get(2)
        .i32_const(1)
        .i32_le_u()
        .if_(BlockType::Empty)
        .local_get(0)
        .local_get(1)
        .call(rt.val_eq)
        .local_get(2)
        .i32_xor()
        .if_(BlockType::Result(ValType::I64))
        .i64_const(VAL_TRUE)
        .else_()
        .i64_const(VAL_FALSE)
        .end()
        .return_()
        .end();

    // Fast path: both IMM_I_SMALL
    s.local_get(0)
        .local_get(1)
        .i64_or()
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .local_get(0)
        .local_get(1)
        .i64_and()
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_I_SMALL_TAG)
        .i64_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(2)
        .i32_const(2)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(0)
        .local_get(1)
        .i64_lt_s()
        .else_()
        .local_get(2)
        .i32_const(3)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(0)
        .local_get(1)
        .i64_le_s()
        .else_()
        .local_get(2)
        .i32_const(4)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(0)
        .local_get(1)
        .i64_gt_s()
        .else_()
        .local_get(0)
        .local_get(1)
        .i64_ge_s()
        .end()
        .end()
        .end()
        .if_(BlockType::Result(ValType::I64))
        .i64_const(VAL_TRUE)
        .else_()
        .i64_const(VAL_FALSE)
        .end()
        .return_()
        .end();

    // Float comparison if either is float
    let check_float = |s: &mut wasm_encoder::InstructionSink<'_>, loc: u32| {
        s.local_get(loc)
            .i64_const(ROT_F64_TAG_MASK)
            .i64_and()
            .i64_const(0)
            .i64_ne()
            .local_get(loc)
            .i64_const(IMM_LOW7_MASK)
            .i64_and()
            .i64_const(IMM_F64_TAG)
            .i64_eq()
            .i32_or()
            .local_get(loc)
            .i64_const(IMM_LOW7_MASK)
            .i64_and()
            .i64_const(IMM_NARROW_TAG)
            .i64_eq()
            .local_get(loc)
            .i64_const(39)
            .i64_shr_u()
            .i64_const(0xf)
            .i64_and()
            .i64_const(NARROW_F32)
            .i64_eq()
            .i32_and()
            .i32_or();
    };
    check_float(&mut s, 0);
    check_float(&mut s, 1);
    s.i32_or()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .local_set(8)
        .local_get(1)
        .call(rt.to_f64)
        .local_set(9)
        .local_get(2)
        .i32_const(2)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(8)
        .local_get(9)
        .f64_lt()
        .else_()
        .local_get(2)
        .i32_const(3)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(8)
        .local_get(9)
        .f64_le()
        .else_()
        .local_get(2)
        .i32_const(4)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(8)
        .local_get(9)
        .f64_gt()
        .else_()
        .local_get(8)
        .local_get(9)
        .f64_ge()
        .end()
        .end()
        .end()
        .if_(BlockType::Result(ValType::I64))
        .i64_const(VAL_TRUE)
        .else_()
        .i64_const(VAL_FALSE)
        .end()
        .return_()
        .end();

    // Lexicographical String comparison if both heap strings
    s.local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(0)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .local_get(1)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(1)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i32_wrap_i64()
        .local_set(4)
        .local_get(1)
        .i32_wrap_i64()
        .local_set(5)
        .local_get(4)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_get(5)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_get(4)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_get(5)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_lt_u()
        .select()
        .local_set(6) // min_len
        .i32_const(0)
        .local_set(7) // i = 0
        .i32_const(0)
        .local_set(3) // cmp_sign = 0
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(7)
        .local_get(6)
        .i32_ge_u()
        .br_if(1)
        .local_get(4)
        .local_get(7)
        .i32_add()
        .i32_load8_u(mem(0, 0))
        .local_get(5)
        .local_get(7)
        .i32_add()
        .i32_load8_u(mem(0, 0))
        .i32_sub()
        .local_tee(3)
        .br_if(1)
        .local_get(7)
        .i32_const(1)
        .i32_add()
        .local_set(7)
        .br(0)
        .end()
        .end()
        .local_get(3)
        .i32_eqz()
        .if_(BlockType::Empty)
        .local_get(4)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_get(5)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_sub()
        .local_set(3)
        .end()
        .local_get(2)
        .i32_const(2)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(3)
        .i32_const(0)
        .i32_lt_s()
        .else_()
        .local_get(2)
        .i32_const(3)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(3)
        .i32_const(0)
        .i32_le_s()
        .else_()
        .local_get(2)
        .i32_const(4)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(3)
        .i32_const(0)
        .i32_gt_s()
        .else_()
        .local_get(3)
        .i32_const(0)
        .i32_ge_s()
        .end()
        .end()
        .end()
        .if_(BlockType::Result(ValType::I64))
        .i64_const(VAL_TRUE)
        .else_()
        .i64_const(VAL_FALSE)
        .end()
        .return_()
        .end();

    // General integer comparison
    s.local_get(0)
        .call(rt.to_i64)
        .local_set(10)
        .local_get(1)
        .call(rt.to_i64)
        .local_set(11)
        .local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_U_SMALL_TAG)
        .i64_eq()
        .local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_USIZE_TAG)
        .i64_eq()
        .i32_or()
        .if_(BlockType::Result(ValType::I32))
        .local_get(2)
        .i32_const(2)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(10)
        .local_get(11)
        .i64_lt_u()
        .else_()
        .local_get(2)
        .i32_const(3)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(10)
        .local_get(11)
        .i64_le_u()
        .else_()
        .local_get(2)
        .i32_const(4)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(10)
        .local_get(11)
        .i64_gt_u()
        .else_()
        .local_get(10)
        .local_get(11)
        .i64_ge_u()
        .end()
        .end()
        .end()
        .else_()
        .local_get(2)
        .i32_const(2)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(10)
        .local_get(11)
        .i64_lt_s()
        .else_()
        .local_get(2)
        .i32_const(3)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(10)
        .local_get(11)
        .i64_le_s()
        .else_()
        .local_get(2)
        .i32_const(4)
        .i32_eq()
        .if_(BlockType::Result(ValType::I32))
        .local_get(10)
        .local_get(11)
        .i64_gt_s()
        .else_()
        .local_get(10)
        .local_get(11)
        .i64_ge_s()
        .end()
        .end()
        .end()
        .end()
        .if_(BlockType::Result(ValType::I64))
        .i64_const(VAL_TRUE)
        .else_()
        .i64_const(VAL_FALSE)
        .end()
        .end();
    codes.function(&f);
}

fn emit_list_new(codes: &mut CodeSection, rt: &RtFuncIds, info: &StaticLayoutInfo) {
    // (param 0=len i32) -> i64
    // List layout (3 words = 24 bytes): [0]=len:u32, [4]=cap:u32, [8]=buf_ptr:u32
    // locals: 1=cap i32, 2=buf i32, 3=wrap i32, 4=i i32
    let mut f = Function::new([(4, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_const(4)
        .local_get(0)
        .i32_const(4)
        .i32_gt_s()
        .select()
        .local_tee(1)
        .i32_const(3)
        .i32_shl()
        .call(rt.alloc)
        .local_set(2)
        // Fill len slots with VAL_UNIT
        .i32_const(0)
        .local_set(4)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(4)
        .local_get(0)
        .i32_ge_s()
        .br_if(1)
        .local_get(2)
        .local_get(4)
        .i32_const(3)
        .i32_shl()
        .i32_add()
        .i64_const(VAL_UNIT)
        .i64_store(mem(0, 3))
        .local_get(4)
        .i32_const(1)
        .i32_add()
        .local_set(4)
        .br(0)
        .end()
        .end()
        .i32_const(info.list_type_idx as i32)
        .i32_const(ROLE_ORDINARY)
        .i32_const(3)
        .i32_const(0)
        .call(rt.alloc_obj)
        .local_tee(3)
        .local_get(0)
        .i32_store(mem(0, 2))
        .local_get(3)
        .local_get(1)
        .i32_store(mem(4, 2))
        .local_get(3)
        .local_get(2)
        .i32_store(mem(8, 2))
        .local_get(3)
        .i64_extend_i32_u()
        .end();
    codes.function(&f);
}

fn emit_list_push(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=list i64, 1=val i64)
    // locals: 2=p i32, 3=len i32, 4=cap i32, 5=buf i32, 6=new_cap i32, 7=new_buf i32
    let mut f = Function::new([(6, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_load(mem(0, 2))
        .local_set(3)
        .local_get(2)
        .i32_load(mem(4, 2))
        .local_set(4)
        .local_get(2)
        .i32_load(mem(8, 2))
        .local_set(5)
        .local_get(3)
        .local_get(4)
        .i32_ge_u()
        .if_(BlockType::Empty)
        .local_get(4)
        .i32_const(1)
        .i32_shl()
        .i32_const(4)
        .local_get(4)
        .i32_const(0)
        .i32_gt_u()
        .select()
        .local_tee(6)
        .i32_const(3)
        .i32_shl()
        .call(rt.alloc)
        .local_set(7)
        .local_get(3)
        .i32_const(0)
        .i32_gt_u()
        .if_(BlockType::Empty)
        .local_get(7)
        .local_get(5)
        .local_get(3)
        .i32_const(3)
        .i32_shl()
        .memory_copy(0, 0)
        .end()
        .local_get(2)
        .local_get(6)
        .i32_store(mem(4, 2))
        .local_get(2)
        .local_get(7)
        .i32_store(mem(8, 2))
        .local_get(7)
        .local_set(5)
        .end()
        .local_get(5)
        .local_get(3)
        .i32_const(3)
        .i32_shl()
        .i32_add()
        .local_get(1)
        .i64_store(mem(0, 3))
        .local_get(2)
        .local_get(3)
        .i32_const(1)
        .i32_add()
        .i32_store(mem(0, 2))
        .end();
    codes.function(&f);
}

fn emit_list_pop(codes: &mut CodeSection) {
    // (param 0=list i64) -> i64 (?Any: VAL_NULL if empty)
    // locals: 1=p i32, 2=len i32
    let mut f = Function::new([(2, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(1)
        .i32_load(mem(0, 2))
        .local_tee(2)
        .i32_eqz()
        .if_(BlockType::Empty)
        .i64_const(VAL_NULL)
        .return_()
        .end()
        .local_get(2)
        .i32_const(1)
        .i32_sub()
        .local_set(2)
        .local_get(1)
        .local_get(2)
        .i32_store(mem(0, 2))
        .local_get(1)
        .i32_load(mem(8, 2))
        .local_get(2)
        .i32_const(3)
        .i32_shl()
        .i32_add()
        .i64_load(mem(0, 3))
        .end();
    codes.function(&f);
}

fn emit_list_get(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=col i64, 1=idx i64) -> i64
    // Supports both List and Map (`LoadIndex` dispatches here; check if `col` is Map via aux==0x4d4150!)
    // locals: 2=p i32, 3=i i32
    let mut f = Function::new([(2, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_const(4)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(0x4d4150)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .local_get(1)
        .call(rt.map_get)
        .return_()
        .end()
        .local_get(1)
        .call(rt.to_i64)
        .i32_wrap_i64()
        .local_tee(3)
        .local_get(2)
        .i32_load(mem(0, 2))
        .i32_ge_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(2)
        .i32_load(mem(8, 2))
        .local_get(3)
        .i32_const(3)
        .i32_shl()
        .i32_add()
        .i64_load(mem(0, 3))
        .end();
    codes.function(&f);
}

fn emit_list_set(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=col i64, 1=idx i64, 2=val i64)
    // locals: 3=p i32, 4=i i32
    let mut f = Function::new([(2, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(3)
        .i32_const(4)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(0x4d4150)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .local_get(1)
        .local_get(2)
        .call(rt.map_set)
        .return_()
        .end()
        .local_get(1)
        .call(rt.to_i64)
        .i32_wrap_i64()
        .local_tee(4)
        .local_get(3)
        .i32_load(mem(0, 2))
        .i32_ge_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(3)
        .i32_load(mem(8, 2))
        .local_get(4)
        .i32_const(3)
        .i32_shl()
        .i32_add()
        .local_get(2)
        .i64_store(mem(0, 3))
        .end();
    codes.function(&f);
}

fn emit_map_new(codes: &mut CodeSection, rt: &RtFuncIds, info: &StaticLayoutInfo) {
    // (param 0=cap i32) -> i64
    // Map layout: header aux = 0x4d4150 ("MAP"), payload 3 words:
    //   [0]=len:u32 (live count), [4]=slots_used:u32, [8]=entries_ptr:u32, [12]=cap:u32
    // Each entry is 24 bytes: [0]=key:i64, [8]=val:i64, [16]=live:i64 (1=live, 0=tombstone)
    // locals: 1=c i32, 2=buf i32, 3=wrap i32
    let mut f = Function::new([(3, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_const(8)
        .local_get(0)
        .i32_const(8)
        .i32_gt_s()
        .select()
        .local_tee(1)
        .i32_const(24)
        .i32_mul()
        .call(rt.alloc)
        .local_set(2)
        .i32_const(info.map_type_idx as i32)
        .i32_const(ROLE_ORDINARY)
        .i32_const(3)
        .i32_const(0x4d4150)
        .call(rt.alloc_obj)
        .local_tee(3)
        .i32_const(0)
        .i32_store(mem(0, 2)) // live len
        .local_get(3)
        .i32_const(0)
        .i32_store(mem(4, 2)) // slots_used
        .local_get(3)
        .local_get(2)
        .i32_store(mem(8, 2)) // entries_ptr
        .local_get(3)
        .local_get(1)
        .i32_store(mem(12, 2)) // cap
        .local_get(3)
        .i64_extend_i32_u()
        .end();
    codes.function(&f);
}

fn emit_map_set(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=map i64, 1=key i64, 2=val i64)
    // locals: 3=p i32, 4=used i32, 5=entries i32, 6=cap i32, 7=i i32, 8=ent i32, 9=new_cap i32, 10=new_ent i32
    let mut f = Function::new([(8, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(3)
        .i32_load(mem(4, 2))
        .local_set(4)
        .local_get(3)
        .i32_load(mem(8, 2))
        .local_set(5)
        .local_get(3)
        .i32_load(mem(12, 2))
        .local_set(6)
        // Search existing live entry
        .i32_const(0)
        .local_set(7)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(7)
        .local_get(4)
        .i32_ge_u()
        .br_if(1)
        .local_get(5)
        .local_get(7)
        .i32_const(24)
        .i32_mul()
        .i32_add()
        .local_tee(8)
        .i64_load(mem(16, 3))
        .i64_const(1)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(8)
        .i64_load(mem(0, 3))
        .local_get(1)
        .call(rt.val_eq)
        .if_(BlockType::Empty)
        .local_get(8)
        .local_get(2)
        .i64_store(mem(8, 3))
        .return_()
        .end()
        .end()
        .local_get(7)
        .i32_const(1)
        .i32_add()
        .local_set(7)
        .br(0)
        .end()
        .end()
        // Append new entry (grow if used >= cap)
        .local_get(4)
        .local_get(6)
        .i32_ge_u()
        .if_(BlockType::Empty)
        .local_get(6)
        .i32_const(1)
        .i32_shl()
        .local_tee(9)
        .i32_const(24)
        .i32_mul()
        .call(rt.alloc)
        .local_tee(10)
        .local_get(5)
        .local_get(4)
        .i32_const(24)
        .i32_mul()
        .memory_copy(0, 0)
        .local_get(3)
        .local_get(10)
        .i32_store(mem(8, 2))
        .local_get(3)
        .local_get(9)
        .i32_store(mem(12, 2))
        .local_get(10)
        .local_set(5)
        .end()
        .local_get(5)
        .local_get(4)
        .i32_const(24)
        .i32_mul()
        .i32_add()
        .local_tee(8)
        .local_get(1)
        .i64_store(mem(0, 3))
        .local_get(8)
        .local_get(2)
        .i64_store(mem(8, 3))
        .local_get(8)
        .i64_const(1)
        .i64_store(mem(16, 3))
        .local_get(3)
        .local_get(4)
        .i32_const(1)
        .i32_add()
        .i32_store(mem(4, 2))
        .local_get(3)
        .local_get(3)
        .i32_load(mem(0, 2))
        .i32_const(1)
        .i32_add()
        .i32_store(mem(0, 2))
        .end();
    codes.function(&f);
}

fn emit_map_get(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=map i64, 1=key i64) -> i64 (VAL_NULL if absent)
    // locals: 2=p i32, 3=used i32, 4=entries i32, 5=i i32, 6=ent i32
    let mut f = Function::new([(5, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_load(mem(4, 2))
        .local_set(3)
        .local_get(2)
        .i32_load(mem(8, 2))
        .local_set(4)
        .i32_const(0)
        .local_set(5)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(5)
        .local_get(3)
        .i32_ge_u()
        .br_if(1)
        .local_get(4)
        .local_get(5)
        .i32_const(24)
        .i32_mul()
        .i32_add()
        .local_tee(6)
        .i64_load(mem(16, 3))
        .i64_const(1)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(6)
        .i64_load(mem(0, 3))
        .local_get(1)
        .call(rt.val_eq)
        .if_(BlockType::Empty)
        .local_get(6)
        .i64_load(mem(8, 3))
        .return_()
        .end()
        .end()
        .local_get(5)
        .i32_const(1)
        .i32_add()
        .local_set(5)
        .br(0)
        .end()
        .end()
        .i64_const(VAL_NULL)
        .end();
    codes.function(&f);
}

fn emit_map_contains(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=map i64, 1=key i64) -> i64 (VAL_TRUE / VAL_FALSE)
    // locals: 2=p i32, 3=used i32, 4=entries i32, 5=i i32, 6=ent i32
    let mut f = Function::new([(5, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_load(mem(4, 2))
        .local_set(3)
        .local_get(2)
        .i32_load(mem(8, 2))
        .local_set(4)
        .i32_const(0)
        .local_set(5)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(5)
        .local_get(3)
        .i32_ge_u()
        .br_if(1)
        .local_get(4)
        .local_get(5)
        .i32_const(24)
        .i32_mul()
        .i32_add()
        .local_tee(6)
        .i64_load(mem(16, 3))
        .i64_const(1)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(6)
        .i64_load(mem(0, 3))
        .local_get(1)
        .call(rt.val_eq)
        .if_(BlockType::Empty)
        .i64_const(VAL_TRUE)
        .return_()
        .end()
        .end()
        .local_get(5)
        .i32_const(1)
        .i32_add()
        .local_set(5)
        .br(0)
        .end()
        .end()
        .i64_const(VAL_FALSE)
        .end();
    codes.function(&f);
}

fn emit_map_remove(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=map i64, 1=key i64) -> i64 (old value or VAL_NULL)
    // locals: 2=p i32, 3=used i32, 4=entries i32, 5=i i32, 6=ent i32
    let mut f = Function::new([(5, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_load(mem(4, 2))
        .local_set(3)
        .local_get(2)
        .i32_load(mem(8, 2))
        .local_set(4)
        .i32_const(0)
        .local_set(5)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(5)
        .local_get(3)
        .i32_ge_u()
        .br_if(1)
        .local_get(4)
        .local_get(5)
        .i32_const(24)
        .i32_mul()
        .i32_add()
        .local_tee(6)
        .i64_load(mem(16, 3))
        .i64_const(1)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(6)
        .i64_load(mem(0, 3))
        .local_get(1)
        .call(rt.val_eq)
        .if_(BlockType::Empty)
        .local_get(6)
        .i64_const(0)
        .i64_store(mem(16, 3))
        .local_get(2)
        .local_get(2)
        .i32_load(mem(0, 2))
        .i32_const(1)
        .i32_sub()
        .i32_store(mem(0, 2))
        .local_get(6)
        .i64_load(mem(8, 3))
        .return_()
        .end()
        .end()
        .local_get(5)
        .i32_const(1)
        .i32_add()
        .local_set(5)
        .br(0)
        .end()
        .end()
        .i64_const(VAL_NULL)
        .end();
    codes.function(&f);
}

fn emit_map_keys(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=map i64) -> i64 (List of keys)
    // locals: 1=list i64, 2=p i32, 3=used i32, 4=entries i32, 5=i i32, 6=ent i32
    let mut f = Function::new([(1, ValType::I64), (5, ValType::I32)]);
    let mut s = f.instructions();
    s.i32_const(0)
        .call(rt.list_new)
        .local_set(1)
        .local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_load(mem(4, 2))
        .local_set(3)
        .local_get(2)
        .i32_load(mem(8, 2))
        .local_set(4)
        .i32_const(0)
        .local_set(5)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(5)
        .local_get(3)
        .i32_ge_u()
        .br_if(1)
        .local_get(4)
        .local_get(5)
        .i32_const(24)
        .i32_mul()
        .i32_add()
        .local_tee(6)
        .i64_load(mem(16, 3))
        .i64_const(1)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(1)
        .local_get(6)
        .i64_load(mem(0, 3))
        .call(rt.list_push)
        .end()
        .local_get(5)
        .i32_const(1)
        .i32_add()
        .local_set(5)
        .br(0)
        .end()
        .end()
        .local_get(1)
        .end();
    codes.function(&f);
}

fn emit_bytes_new(codes: &mut CodeSection, rt: &RtFuncIds, info: &StaticLayoutInfo) {
    // (param 0=len i32) -> i64
    // Bytes payload (2 words = 16 bytes): [0]=data_ptr:u32, [4]=len:u32, [8]=cap:u32
    // locals: 1=cap i32, 2=buf i32, 3=wrap i32
    let mut f = Function::new([(3, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_const(16)
        .local_get(0)
        .i32_const(16)
        .i32_gt_s()
        .select()
        .local_tee(1)
        .call(rt.alloc)
        .local_set(2)
        .local_get(0)
        .i32_const(0)
        .i32_gt_s()
        .if_(BlockType::Empty)
        .local_get(2)
        .i32_const(0)
        .local_get(0)
        .memory_fill(0)
        .end()
        .i32_const(info.bytes_type_idx as i32)
        .i32_const(ROLE_BYTES)
        .i32_const(2)
        .i32_const(0)
        .call(rt.alloc_obj)
        .local_tee(3)
        .local_get(2)
        .i32_store(mem(0, 2))
        .local_get(3)
        .local_get(0)
        .i32_store(mem(4, 2))
        .local_get(3)
        .local_get(1)
        .i32_store(mem(8, 2))
        .local_get(3)
        .i64_extend_i32_u()
        .end();
    codes.function(&f);
}

fn emit_bytes_from_str(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=str_val i64) -> i64
    // locals: 1=sp i32, 2=len i32, 3=b i64
    let mut f = Function::new([(2, ValType::I32), (1, ValType::I64)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(1)
        .i32_const(8)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_tee(2)
        .call(rt.bytes_new)
        .local_tee(3)
        .i32_wrap_i64()
        .i32_load(mem(0, 2))
        .local_get(1)
        .local_get(2)
        .memory_copy(0, 0)
        .local_get(3)
        .end();
    codes.function(&f);
}

fn emit_bytes_to_str(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=bytes_val i64) -> i64
    // local 1=bp i32
    let mut f = Function::new([(1, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(1)
        .i32_load(mem(0, 2))
        .local_get(1)
        .i32_load(mem(4, 2))
        .call(rt.alloc_str)
        .end();
    codes.function(&f);
}

fn emit_bytes_get(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=b i64, 1=idx i64) -> i64 (U8 narrow immediate)
    // locals: 2=bp i32, 3=i i32
    let mut f = Function::new([(2, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_set(2)
        .local_get(1)
        .call(rt.to_i64)
        .i32_wrap_i64()
        .local_tee(3)
        .local_get(2)
        .i32_load(mem(4, 2))
        .i32_ge_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(2)
        .i32_load(mem(0, 2))
        .local_get(3)
        .i32_add()
        .i32_load8_u(mem(0, 0))
        .i64_extend_i32_u()
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_NARROW_TAG) // NARROW_U8 == 0!
        .i64_or()
        .end();
    codes.function(&f);
}

fn emit_bytes_set(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=b i64, 1=idx i64, 2=v i64)
    // locals: 3=bp i32, 4=i i32
    let mut f = Function::new([(2, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_set(3)
        .local_get(1)
        .call(rt.to_i64)
        .i32_wrap_i64()
        .local_tee(4)
        .local_get(3)
        .i32_load(mem(4, 2))
        .i32_ge_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(3)
        .i32_load(mem(0, 2))
        .local_get(4)
        .i32_add()
        .local_get(2)
        .call(rt.to_i64)
        .i32_wrap_i64()
        .i32_store8(mem(0, 0))
        .end();
    codes.function(&f);
}

fn emit_bytes_push(codes: &mut CodeSection, rt: &RtFuncIds) {
    // (param 0=b i64, 1=v i64)
    // locals: 2=bp i32, 3=len i32, 4=cap i32, 5=buf i32, 6=new_cap i32, 7=new_buf i32
    let mut f = Function::new([(6, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i32_wrap_i64()
        .local_tee(2)
        .i32_load(mem(0, 2))
        .local_set(5)
        .local_get(2)
        .i32_load(mem(4, 2))
        .local_set(3)
        .local_get(2)
        .i32_load(mem(8, 2))
        .local_set(4)
        .local_get(3)
        .local_get(4)
        .i32_ge_u()
        .if_(BlockType::Empty)
        .local_get(4)
        .i32_const(1)
        .i32_shl()
        .i32_const(16)
        .local_get(4)
        .i32_const(0)
        .i32_gt_u()
        .select()
        .local_tee(6)
        .call(rt.alloc)
        .local_tee(7)
        .local_get(5)
        .local_get(3)
        .memory_copy(0, 0)
        .local_get(2)
        .local_get(7)
        .i32_store(mem(0, 2))
        .local_get(2)
        .local_get(6)
        .i32_store(mem(8, 2))
        .local_get(7)
        .local_set(5)
        .end()
        .local_get(5)
        .local_get(3)
        .i32_add()
        .local_get(1)
        .call(rt.to_i64)
        .i32_wrap_i64()
        .i32_store8(mem(0, 0))
        .local_get(2)
        .local_get(3)
        .i32_const(1)
        .i32_add()
        .i32_store(mem(4, 2))
        .end();
    codes.function(&f);
}

fn emit_reflected_type(codes: &mut CodeSection, info: &StaticLayoutInfo) {
    // (param 0=v i64) -> i32 (canonical TypeIndex)
    // locals: 1=low7 i64, 2=kind i32, 3=raw_ty i32
    let mut f = Function::new([(1, ValType::I64), (2, ValType::I32)]);
    let mut s = f.instructions();
    // Rotated f64 -> TY_F64
    s.local_get(0)
        .i64_const(ROT_F64_TAG_MASK)
        .i64_and()
        .i64_const(0)
        .i64_ne()
        .if_(BlockType::Empty)
        .i32_const(TY_F64 as i32)
        .return_()
        .end();
    // Heap object -> read type_index at ptr - 16, then canonicalize via type_meta_addr
    s.local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(0)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i32_wrap_i64()
        .i32_const(16)
        .i32_sub()
        .i32_load(mem(0, 2))
        .local_tee(3)
        .i32_const(info.type_count as i32)
        .i32_lt_u()
        .if_(BlockType::Result(ValType::I32))
        .i32_const(info.type_meta_addr as i32)
        .local_get(3)
        .i32_const(5)
        .i32_shl()
        .i32_add()
        .i32_load(mem(0, 2))
        .else_()
        .local_get(3)
        .end()
        .return_()
        .end();

    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .local_set(1);

    let map_imm = [
        (IMM_I_SMALL_TAG, TY_I64),
        (IMM_U_SMALL_TAG, TY_U64),
        (IMM_F64_TAG, TY_F64),
        (IMM_BOOL_TAG, TY_BOOL),
        (IMM_NULL_TAG, info.null_type_idx),
        (IMM_UNIT_TAG, TY_UNIT),
        (IMM_CHAR_TAG, TY_CHAR),
        (IMM_TYPE_TAG, TY_TYPE),
        (IMM_USIZE_TAG, TY_USIZE),
        (IMM_ISIZE_TAG, TY_ISIZE),
    ];
    for (tag, ty) in map_imm {
        s.local_get(1)
            .i64_const(tag)
            .i64_eq()
            .if_(BlockType::Empty)
            .i32_const(ty as i32)
            .return_()
            .end();
    }
    // IMM_ENUM: ((val >> 7) >> 25) == val >> 32
    s.local_get(1)
        .i64_const(IMM_ENUM_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(32)
        .i64_shr_u()
        .i32_wrap_i64()
        .return_()
        .end();
    // IMM_NARROW
    s.local_get(1)
        .i64_const(IMM_NARROW_TAG)
        .i64_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(39)
        .i64_shr_u()
        .i32_wrap_i64()
        .i32_const(0xf)
        .i32_and()
        .local_set(2);
    for (k, ty) in [
        (NARROW_U8 as i32, TY_U8),
        (NARROW_U16 as i32, TY_U16),
        (NARROW_U32 as i32, TY_U32),
        (NARROW_I8 as i32, TY_I8),
        (NARROW_I16 as i32, TY_I16),
        (NARROW_I32 as i32, TY_I32),
        (NARROW_F32 as i32, TY_F32),
    ] {
        s.local_get(2)
            .i32_const(k)
            .i32_eq()
            .if_(BlockType::Empty)
            .i32_const(ty as i32)
            .return_()
            .end();
    }
    s.end();
    s.i32_const(TY_ANY as i32).end();
    codes.function(&f);
}

fn emit_find_method(codes: &mut CodeSection, info: &StaticLayoutInfo) {
    // (param 0=recv_ty i32, 1=method_str_id i32) -> i64 ((self_proof_view << 32) | func_id, or -1)
    // Method table entry (16 bytes): [0]=canon_ty:u32, [4]=method_str_id:u32, [8]=func_id:u32, [12]=self_proof_view:u32
    // locals: 2=i i32, 3=ent i32
    let mut f = Function::new([(2, ValType::I32)]);
    let mut s = f.instructions();
    s.i32_const(0)
        .local_set(2)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(2)
        .i32_const(info.method_count as i32)
        .i32_ge_u()
        .br_if(1)
        .i32_const(info.method_table_addr as i32)
        .local_get(2)
        .i32_const(4)
        .i32_shl()
        .i32_add()
        .local_tee(3)
        .i32_load(mem(0, 2))
        .local_get(0)
        .i32_eq()
        .local_get(3)
        .i32_load(mem(4, 2))
        .local_get(1)
        .i32_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(3)
        .i32_load(mem(12, 2))
        .i64_extend_i32_u()
        .i64_const(32)
        .i64_shl()
        .local_get(3)
        .i32_load(mem(8, 2))
        .i64_extend_i32_u()
        .i64_or()
        .return_()
        .end()
        .local_get(2)
        .i32_const(1)
        .i32_add()
        .local_set(2)
        .br(0)
        .end()
        .end()
        .i64_const(-1)
        .end();
    codes.function(&f);
}

fn emit_trait_proof(codes: &mut CodeSection, rt: &RtFuncIds, info: &StaticLayoutInfo) {
    // (param 0=recv_or_proof i64, 1=view i32) -> i64 (IMM_TRAIT_PROOF = ((impl_idx + 1) << 7) | 0x51)
    // TraitImplTable entry (16 bytes): [0]=impl_ty:u32, [4]=trait_ty:u32, [8]=vtable_start:u32, [12]=slot_count:u32
    // locals: 2=concrete i32, 3=canon_view i32, 4=i i32, 5=ent i32
    let mut f = Function::new([(4, ValType::I32)]);
    let mut s = f.instructions();
    // Canonicalize `view`
    s.local_get(1)
        .i32_const(info.type_count as i32)
        .i32_lt_u()
        .if_(BlockType::Result(ValType::I32))
        .i32_const(info.type_meta_addr as i32)
        .local_get(1)
        .i32_const(5)
        .i32_shl()
        .i32_add()
        .i32_load(mem(0, 2))
        .else_()
        .local_get(1)
        .end()
        .local_set(3);
    // If `recv_or_proof` is already an IMM_TRAIT_PROOF (used by TraitProject!), extract `impl_ty` from its table row!
    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_TRAIT_PROOF_TAG)
        .i64_eq()
        .if_(BlockType::Result(ValType::I32))
        .i32_const(info.trait_impl_addr as i32)
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .i32_wrap_i64()
        .i32_const(1)
        .i32_sub()
        .i32_const(4)
        .i32_shl()
        .i32_add()
        .i32_load(mem(0, 2))
        .else_()
        .local_get(0)
        .call(rt.reflected_type)
        .end()
        .local_set(2);

    s.i32_const(0)
        .local_set(4)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(4)
        .i32_const(info.trait_impl_count as i32)
        .i32_ge_u()
        .br_if(1)
        .i32_const(info.trait_impl_addr as i32)
        .local_get(4)
        .i32_const(4)
        .i32_shl()
        .i32_add()
        .local_tee(5)
        .i32_load(mem(0, 2))
        .local_get(2)
        .i32_eq()
        .local_get(5)
        .i32_load(mem(4, 2))
        .local_get(3)
        .i32_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(4)
        .i32_const(1)
        .i32_add()
        .i64_extend_i32_u()
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_TRAIT_PROOF_TAG)
        .i64_or()
        .return_()
        .end()
        .local_get(4)
        .i32_const(1)
        .i32_add()
        .local_set(4)
        .br(0)
        .end()
        .end()
        .unreachable()
        .end();
    codes.function(&f);
}

fn emit_trait_vtable_func(codes: &mut CodeSection, info: &StaticLayoutInfo) {
    // (param 0=proof i64, 1=slot i32) -> i64 ((self_proof_view << 32) | func_id)
    // VTable entry (8 bytes): [0]=func_id:u32, [4]=self_proof_view:u32
    // locals: 2=row_idx i32, 3=row_ptr i32, 4=vt_ptr i32
    let mut f = Function::new([(3, ValType::I32)]);
    let mut s = f.instructions();
    s.local_get(0)
        .i64_const(IMM_LOW7_MASK)
        .i64_and()
        .i64_const(IMM_TRAIT_PROOF_TAG)
        .i64_ne()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .local_get(0)
        .i64_const(7)
        .i64_shr_u()
        .i32_wrap_i64()
        .i32_const(1)
        .i32_sub()
        .local_tee(2)
        .i32_const(info.trait_impl_count as i32)
        .i32_ge_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .i32_const(info.trait_impl_addr as i32)
        .local_get(2)
        .i32_const(4)
        .i32_shl()
        .i32_add()
        .local_tee(3)
        .i32_load(mem(12, 2))
        .local_get(1)
        .i32_le_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .i32_const(info.vtable_entries_addr as i32)
        .local_get(3)
        .i32_load(mem(8, 2))
        .local_get(1)
        .i32_add()
        .i32_const(3)
        .i32_shl()
        .i32_add()
        .local_tee(4)
        .i32_load(mem(4, 2))
        .i64_extend_i32_u()
        .i64_const(32)
        .i64_shl()
        .local_get(4)
        .i32_load(mem(0, 2))
        .i64_extend_i32_u()
        .i64_or()
        .end();
    codes.function(&f);
}

fn emit_type_check(codes: &mut CodeSection, rt: &RtFuncIds, info: &StaticLayoutInfo) {
    // (param 0=val i64, 1=target_ty i32) -> i32 (1 or 0)
    // locals: 2=canon i32, 3=meta i32, 4=kind i32, 5=actual i32, 6=i i32, 7=ent i32
    let mut f = Function::new([(6, ValType::I32)]);
    let mut s = f.instructions();
    // Canonicalize target_ty
    s.local_get(1)
        .i32_const(info.type_count as i32)
        .i32_ge_u()
        .if_(BlockType::Empty)
        .i32_const(0)
        .return_()
        .end()
        .i32_const(info.type_meta_addr as i32)
        .local_get(1)
        .i32_const(5)
        .i32_shl()
        .i32_add()
        .i32_load(mem(0, 2))
        .local_set(2);

    // Any matches everything; NoReturn matches nothing
    s.local_get(2)
        .i32_const(TY_ANY as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .i32_const(1)
        .return_()
        .end()
        .local_get(2)
        .i32_const(TY_NORETURN as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .i32_const(0)
        .return_()
        .end();

    s.i32_const(info.type_meta_addr as i32)
        .local_get(2)
        .i32_const(5)
        .i32_shl()
        .i32_add()
        .local_tee(3)
        .i32_load(mem(4, 2))
        .local_set(4);

    // TK_OPTIONAL: null -> 1, else recurse on inner (`count_or_inner`)
    s.local_get(4)
        .i32_const(TK_OPTIONAL as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(VAL_NULL)
        .i64_eq()
        .if_(BlockType::Empty)
        .i32_const(1)
        .return_()
        .end()
        .local_get(0)
        .local_get(3)
        .i32_load(mem(8, 2))
        .call(rt.type_check)
        .return_()
        .end();

    // Null does not match non-optional, non-Any
    s.local_get(0)
        .i64_const(VAL_NULL)
        .i64_eq()
        .if_(BlockType::Empty)
        .i32_const(0)
        .return_()
        .end();

    s.local_get(0).call(rt.reflected_type).local_set(5);

    // Exact canonical match
    s.local_get(5)
        .local_get(2)
        .i32_eq()
        .if_(BlockType::Empty)
        .i32_const(1)
        .return_()
        .end();

    // TK_TRAIT: check if `(actual, canon)` exists in `TraitImplTable`!
    s.local_get(4)
        .i32_const(TK_TRAIT as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .i32_const(0)
        .local_set(6)
        .block(BlockType::Empty)
        .loop_(BlockType::Empty)
        .local_get(6)
        .i32_const(info.trait_impl_count as i32)
        .i32_ge_u()
        .br_if(1)
        .i32_const(info.trait_impl_addr as i32)
        .local_get(6)
        .i32_const(4)
        .i32_shl()
        .i32_add()
        .local_tee(7)
        .i32_load(mem(0, 2))
        .local_get(5)
        .i32_eq()
        .local_get(7)
        .i32_load(mem(4, 2))
        .local_get(2)
        .i32_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .i32_const(1)
        .return_()
        .end()
        .local_get(6)
        .i32_const(1)
        .i32_add()
        .local_set(6)
        .br(0)
        .end()
        .end()
        .i32_const(0)
        .return_()
        .end();

    // TK_FUNCTION: any closure matches Function shape dynamically
    s.local_get(4)
        .i32_const(TK_FUNCTION as i32)
        .i32_eq()
        .local_get(5)
        .i32_const(TY_CLOSURE as i32)
        .i32_eq()
        .i32_and()
        .if_(BlockType::Empty)
        .i32_const(1)
        .return_()
        .end();

    s.i32_const(0).end();
    codes.function(&f);
}

fn emit_type_cast(codes: &mut CodeSection, rt: &RtFuncIds, info: &StaticLayoutInfo) {
    // (param 0=val i64, 1=target_ty i32, 2=mode i32) -> i64
    // mode: 0=TypeAssert, 1=TypeCast, 2=TypeCastSafe (returns VAL_NULL on failure)
    // locals: 3=canon i32, 4=meta i32, 5=kind i32, 6=n i64, 7=f f64, 8=ptr i32
    let mut f = Function::new([(4, ValType::I32), (1, ValType::I64), (1, ValType::F64)]);
    // locals: 3,4,5,6 = i32; 7 = i64; 8 = f64
    let mut s = f.instructions();
    s.local_get(1)
        .i32_const(info.type_count as i32)
        .i32_ge_u()
        .if_(BlockType::Empty)
        .unreachable()
        .end()
        .i32_const(info.type_meta_addr as i32)
        .local_get(1)
        .i32_const(5)
        .i32_shl()
        .i32_add()
        .i32_load(mem(0, 2))
        .local_set(3);

    // If already matches target_ty, and not numeric re-tagging cast:
    s.local_get(3)
        .i32_const(TY_ANY as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .return_()
        .end();

    s.i32_const(info.type_meta_addr as i32)
        .local_get(3)
        .i32_const(5)
        .i32_shl()
        .i32_add()
        .local_tee(4)
        .i32_load(mem(4, 2))
        .local_set(5);

    // Optional: if null -> null; else recurse on inner
    s.local_get(5)
        .i32_const(TK_OPTIONAL as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(VAL_NULL)
        .i64_eq()
        .if_(BlockType::Empty)
        .i64_const(VAL_NULL)
        .return_()
        .end()
        .local_get(0)
        .local_get(4)
        .i32_load(mem(8, 2))
        .local_get(2)
        .call(rt.type_cast)
        .return_()
        .end();

    // ErrorQualified: if already an ErrorEnvelope heap object, return as-is; otherwise wrap in Ok envelope!
    s.local_get(5)
        .i32_const(TK_ERROR as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .i64_const(TAG_MASK)
        .i64_and()
        .i64_eqz()
        .local_get(0)
        .i64_const(0)
        .i64_ne()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .i32_wrap_i64()
        .i32_const(12)
        .i32_sub()
        .i32_load(mem(0, 2))
        .i32_const(ROLE_ERROR_ENVELOPE)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .return_()
        .end()
        .end()
        // Wrap `val` (cast to inner) as Ok(0, 0, payload)
        .local_get(0)
        .local_get(4)
        .i32_load(mem(8, 2))
        .local_get(2)
        .call(rt.type_cast)
        .local_set(7)
        .local_get(3)
        .i32_const(ROLE_ERROR_ENVELOPE)
        .i32_const(3)
        .i32_const(0)
        .call(rt.alloc_obj)
        .local_tee(6)
        .i64_const(0)
        .i64_store(mem(0, 3))
        .local_get(6)
        .i64_const(0)
        .i64_store(mem(8, 3))
        .local_get(6)
        .local_get(7)
        .i64_store(mem(16, 3))
        .local_get(6)
        .i64_extend_i32_u()
        .return_()
        .end();

    // Numeric target (0..=13) when mode != 0 (explicit `as` / `as?`) or compatible numeric subtype
    s.local_get(3)
        .i32_const(TY_F64 as i32)
        .i32_le_u()
        .local_get(2)
        .i32_const(0)
        .i32_ne()
        .local_get(0)
        .call(rt.reflected_type)
        .local_get(3)
        .i32_eq()
        .i32_or()
        .i32_and()
        .if_(BlockType::Empty);
    // Exact reflected type already matches -> return val!
    s.local_get(0)
        .call(rt.reflected_type)
        .local_get(3)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .return_()
        .end();
    // F64
    s.local_get(3)
        .i32_const(TY_F64 as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .call(rt.from_f64)
        .return_()
        .end();
    // F32
    s.local_get(3)
        .i32_const(TY_F32 as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(0)
        .call(rt.to_f64)
        .f32_demote_f64()
        .i32_reinterpret_f32()
        .i64_extend_i32_u()
        .i64_const(NARROW_F32 << 32)
        .i64_or()
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_NARROW_TAG)
        .i64_or()
        .return_()
        .end();
    // Integer casts
    s.local_get(0).call(rt.to_i64).local_set(7);
    s.local_get(3)
        .i32_const(TY_I64 as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(7)
        .call(rt.from_i64)
        .return_()
        .end();
    s.local_get(3)
        .i32_const(TY_U64 as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(7)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_U_SMALL_TAG)
        .i64_or()
        .return_()
        .end();
    s.local_get(3)
        .i32_const(TY_USIZE as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(7)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_USIZE_TAG)
        .i64_or()
        .return_()
        .end();
    s.local_get(3)
        .i32_const(TY_ISIZE as i32)
        .i32_eq()
        .if_(BlockType::Empty)
        .local_get(7)
        .i64_const(7)
        .i64_shl()
        .i64_const(IMM_ISIZE_TAG)
        .i64_or()
        .return_()
        .end();
    for (ty, narrow_k, mask) in [
        (TY_U8, NARROW_U8, 0xff_i64),
        (TY_U16, NARROW_U16, 0xffff_i64),
        (TY_U32, NARROW_U32, 0xffff_ffff_i64),
        (TY_I8, NARROW_I8, 0xffff_ffff_i64),
        (TY_I16, NARROW_I16, 0xffff_ffff_i64),
        (TY_I32, NARROW_I32, 0xffff_ffff_i64),
    ] {
        s.local_get(3)
            .i32_const(ty as i32)
            .i32_eq()
            .if_(BlockType::Empty)
            .local_get(7)
            .i64_const(mask)
            .i64_and()
            .i64_const(narrow_k << 32)
            .i64_or()
            .i64_const(7)
            .i64_shl()
            .i64_const(IMM_NARROW_TAG)
            .i64_or()
            .return_()
            .end();
    }
    s.end();

    // Newtype wrap/unwrap
    s.local_get(5)
        .i32_const(TK_NEWTYPE as i32)
        .i32_eq()
        .local_get(0)
        .call(rt.reflected_type)
        .local_get(3)
        .i32_ne()
        .i32_and()
        .if_(BlockType::Empty)
        .local_get(0)
        .local_get(4)
        .i32_load(mem(8, 2))
        .local_get(2)
        .call(rt.type_cast)
        .local_set(7)
        .local_get(3)
        .i32_const(ROLE_ORDINARY)
        .i32_const(1)
        .i32_const(0)
        .call(rt.alloc_obj)
        .local_tee(6)
        .local_get(7)
        .i64_store(mem(0, 3))
        .local_get(6)
        .i64_extend_i32_u()
        .return_()
        .end();

    // Fallback check via type_check
    s.local_get(0)
        .local_get(3)
        .call(rt.type_check)
        .if_(BlockType::Result(ValType::I64))
        .local_get(0)
        .else_()
        .local_get(2)
        .i32_const(2)
        .i32_eq()
        .if_(BlockType::Result(ValType::I64))
        .i64_const(VAL_NULL)
        .else_()
        .unreachable()
        .end()
        .end()
        .end();
    codes.function(&f);
}

fn emit_trap_unreachable(codes: &mut CodeSection) {
    // Dead/pruned function stub matching `(param i32 i32) -> (result i64)`
    let mut f = Function::new([]);
    f.instructions().unreachable().end();
    codes.function(&f);
}
