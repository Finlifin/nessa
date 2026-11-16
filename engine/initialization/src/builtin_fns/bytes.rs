//! Binary packing and unpacking builtins (`__bytes_pack`, `__bytes_packsize`, `__bytes_unpack`).

use interpreter::{BuiltinCtx, NativeValue, VmError};
use runtime::Number;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn native() -> Self {
        if cfg!(target_endian = "little") {
            Self::Little
        } else {
            Self::Big
        }
    }
}

#[derive(Clone, Copy)]
struct PackHeader {
    endian: Endian,
    max_align: usize,
}

impl Default for PackHeader {
    fn default() -> Self {
        Self {
            endian: Endian::native(),
            max_align: 1,
        }
    }
}

#[derive(Clone, Copy)]
enum PackOp {
    SignedInt(usize),
    UnsignedInt { size: usize, return_u64: bool },
    F32,
    F64,
    FixedBytes(usize),
    ZeroTermStr,
    SizedStr(usize),
    PaddingByte,
    AlignTo(usize),
    Noop,
}

impl PackOp {
    fn natural_align(self) -> usize {
        match self {
            Self::SignedInt(sz) | Self::UnsignedInt { size: sz, .. } | Self::SizedStr(sz) => sz,
            Self::F32 => 4,
            Self::F64 => 8,
            Self::AlignTo(sz) => sz,
            Self::FixedBytes(_) | Self::ZeroTermStr | Self::PaddingByte | Self::Noop => 1,
        }
    }
}

fn compute_padding(pos: usize, op: PackOp, max_align: usize) -> Result<usize, String> {
    let mut align = op.natural_align().min(max_align);
    if matches!(op, PackOp::AlignTo(_)) {
        align = op.natural_align().min(max_align);
    }
    if align <= 1 {
        return Ok(0);
    }
    if !align.is_power_of_two() {
        return Err(format!("alignment {align} is not a power of 2"));
    }
    Ok((align - (pos & (align - 1))) & (align - 1))
}

fn parse_uint_opt(
    fmt: &[u8],
    i: &mut usize,
    default: Option<usize>,
    min_v: usize,
    max_v: usize,
    opt_name: char,
) -> Result<usize, String> {
    if *i >= fmt.len() || !fmt[*i].is_ascii_digit() {
        return default.ok_or_else(|| format!("missing size for format option '{opt_name}'"));
    }
    let mut val = 0usize;
    while *i < fmt.len() && fmt[*i].is_ascii_digit() {
        val = val
            .checked_mul(10)
            .and_then(|v| v.checked_add(usize::from(fmt[*i] - b'0')))
            .ok_or_else(|| format!("size overflow in format option '{opt_name}'"))?;
        *i += 1;
    }
    if val < min_v || val > max_v {
        return Err(format!(
            "integral size ({val}) out of limits [{min_v}, {max_v}]"
        ));
    }
    Ok(val)
}

fn next_pack_op(fmt: &[u8], i: &mut usize, hdr: &mut PackHeader) -> Result<Option<PackOp>, String> {
    while *i < fmt.len() && fmt[*i] == b' ' {
        *i += 1;
    }
    if *i >= fmt.len() {
        return Ok(None);
    }
    let ch = fmt[*i];
    *i += 1;
    match ch {
        b'<' => {
            hdr.endian = Endian::Little;
            Ok(Some(PackOp::Noop))
        }
        b'>' => {
            hdr.endian = Endian::Big;
            Ok(Some(PackOp::Noop))
        }
        b'=' => {
            hdr.endian = Endian::native();
            Ok(Some(PackOp::Noop))
        }
        b'!' => {
            let native_max = std::mem::align_of::<u64>();
            let align = parse_uint_opt(fmt, i, Some(native_max), 1, 16, '!')?;
            if !align.is_power_of_two() {
                return Err(format!("alignment {align} is not a power of 2"));
            }
            hdr.max_align = align;
            Ok(Some(PackOp::Noop))
        }
        b'b' => Ok(Some(PackOp::SignedInt(1))),
        b'B' => Ok(Some(PackOp::UnsignedInt {
            size: 1,
            return_u64: false,
        })),
        b'h' => Ok(Some(PackOp::SignedInt(2))),
        b'H' => Ok(Some(PackOp::UnsignedInt {
            size: 2,
            return_u64: false,
        })),
        b'l' => Ok(Some(PackOp::SignedInt(8))),
        b'L' => Ok(Some(PackOp::UnsignedInt {
            size: 8,
            return_u64: true,
        })),
        b'j' => Ok(Some(PackOp::SignedInt(8))),
        b'J' => Ok(Some(PackOp::UnsignedInt {
            size: 8,
            return_u64: true,
        })),
        b'T' => Ok(Some(PackOp::UnsignedInt {
            size: std::mem::size_of::<usize>(),
            return_u64: true,
        })),
        b'i' => {
            let sz = parse_uint_opt(fmt, i, Some(4), 1, 16, 'i')?;
            Ok(Some(PackOp::SignedInt(sz)))
        }
        b'I' => {
            let sz = parse_uint_opt(fmt, i, Some(4), 1, 16, 'I')?;
            Ok(Some(PackOp::UnsignedInt {
                size: sz,
                return_u64: sz >= 8,
            }))
        }
        b'f' => Ok(Some(PackOp::F32)),
        b'd' | b'n' => Ok(Some(PackOp::F64)),
        b'c' => {
            let sz = parse_uint_opt(fmt, i, None, 0, u16::MAX as usize, 'c')?;
            Ok(Some(PackOp::FixedBytes(sz)))
        }
        b'z' => Ok(Some(PackOp::ZeroTermStr)),
        b's' => {
            let sz = parse_uint_opt(fmt, i, Some(8), 1, 16, 's')?;
            Ok(Some(PackOp::SizedStr(sz)))
        }
        b'x' => Ok(Some(PackOp::PaddingByte)),
        b'X' => {
            if *i >= fmt.len() {
                return Err("invalid next option for option 'X'".to_string());
            }
            let mut tmp_hdr = *hdr;
            let inner = next_pack_op(fmt, i, &mut tmp_hdr)?;
            match inner {
                Some(
                    op @ (PackOp::SignedInt(_)
                    | PackOp::UnsignedInt { .. }
                    | PackOp::F32
                    | PackOp::F64
                    | PackOp::SizedStr(_)),
                ) => Ok(Some(PackOp::AlignTo(op.natural_align()))),
                _ => Err("invalid next option for option 'X'".to_string()),
            }
        }
        other => Err(format!("invalid format option '{}'", other as char)),
    }
}

/// `582` `__bytes_pack(fmt: String, values: List) -> (List, String)`
pub fn bytes_pack(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let fmt_str = ctx.arg_string(0)?;
    let values_root = ctx.arg_rooted(1)?;
    let values_val = ctx.reload_root(&values_root)?;
    let values_len = ctx.list_len(values_val)?;

    let fmt = fmt_str.as_bytes();
    let mut i = 0usize;
    let mut hdr = PackHeader::default();
    let mut out: Vec<u8> = Vec::new();
    let mut val_idx = 0usize;

    while let Some(op) = match next_pack_op(fmt, &mut i, &mut hdr) {
        Ok(op) => op,
        Err(msg) => return return_pack_result(ctx, Vec::new(), &msg),
    } {
        if matches!(op, PackOp::Noop) {
            continue;
        }
        let pad = match compute_padding(out.len(), op, hdr.max_align) {
            Ok(p) => p,
            Err(msg) => return return_pack_result(ctx, Vec::new(), &msg),
        };
        if out.len().saturating_add(pad) > u16::MAX as usize {
            return Err(VmError::ObjectTooLarge);
        }
        out.resize(out.len() + pad, 0);

        match op {
            PackOp::Noop | PackOp::AlignTo(_) => {}
            PackOp::PaddingByte => {
                if out.len() >= u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                out.push(0);
            }
            PackOp::SignedInt(sz) => {
                if val_idx >= values_len {
                    return return_pack_result(ctx, Vec::new(), "missing value for pack format");
                }
                let elem_root = ctx.list_get_rooted(&values_root, val_idx)?;
                val_idx += 1;
                let elem = ctx.reload_root(&elem_root)?;
                let Ok(num) = ctx.number_of(elem) else {
                    return return_pack_result(ctx, Vec::new(), "expected integer for signed pack");
                };
                let signed: i128 = match num {
                    Number::I64(v) => i128::from(v),
                    Number::U64(v) => i128::from(v),
                    Number::I128(v) => v,
                    Number::U128(v) => match i128::try_from(v) {
                        Ok(s) => s,
                        Err(_) => {
                            return return_pack_result(ctx, Vec::new(), "integer overflow");
                        }
                    },
                    Number::F64(_) => {
                        return return_pack_result(
                            ctx,
                            Vec::new(),
                            "expected integer for signed pack, got float",
                        );
                    }
                };
                if sz < 16 {
                    let min_v = -(1i128 << (sz * 8 - 1));
                    let max_v = (1i128 << (sz * 8 - 1)) - 1;
                    if signed < min_v || signed > max_v {
                        return return_pack_result(ctx, Vec::new(), "integer overflow");
                    }
                }
                if out.len().saturating_add(sz) > u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                write_uint_bytes(&mut out, signed as u128, sz, hdr.endian);
            }
            PackOp::UnsignedInt { size: sz, .. } => {
                if val_idx >= values_len {
                    return return_pack_result(ctx, Vec::new(), "missing value for pack format");
                }
                let elem_root = ctx.list_get_rooted(&values_root, val_idx)?;
                val_idx += 1;
                let elem = ctx.reload_root(&elem_root)?;
                let Ok(num) = ctx.number_of(elem) else {
                    return return_pack_result(
                        ctx,
                        Vec::new(),
                        "expected integer for unsigned pack",
                    );
                };
                let unsigned: u128 = match num {
                    Number::I64(v) => match u128::try_from(v) {
                        Ok(u) => u,
                        Err(_) => return return_pack_result(ctx, Vec::new(), "unsigned overflow"),
                    },
                    Number::U64(v) => u128::from(v),
                    Number::I128(v) => match u128::try_from(v) {
                        Ok(u) => u,
                        Err(_) => return return_pack_result(ctx, Vec::new(), "unsigned overflow"),
                    },
                    Number::U128(v) => v,
                    Number::F64(_) => {
                        return return_pack_result(
                            ctx,
                            Vec::new(),
                            "expected integer for unsigned pack, got float",
                        );
                    }
                };
                if sz < 16 {
                    let max_v = (1u128 << (sz * 8)) - 1;
                    if unsigned > max_v {
                        return return_pack_result(ctx, Vec::new(), "unsigned overflow");
                    }
                }
                if out.len().saturating_add(sz) > u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                write_uint_bytes(&mut out, unsigned, sz, hdr.endian);
            }
            PackOp::F32 => {
                if val_idx >= values_len {
                    return return_pack_result(ctx, Vec::new(), "missing value for pack format");
                }
                let elem_root = ctx.list_get_rooted(&values_root, val_idx)?;
                val_idx += 1;
                let elem = ctx.reload_root(&elem_root)?;
                let Ok(num) = ctx.number_of(elem) else {
                    return return_pack_result(ctx, Vec::new(), "expected number for 'f'");
                };
                let v = num.to_f64() as f32;
                let bytes = match hdr.endian {
                    Endian::Little => v.to_le_bytes(),
                    Endian::Big => v.to_be_bytes(),
                };
                if out.len().saturating_add(4) > u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                out.extend_from_slice(&bytes);
            }
            PackOp::F64 => {
                if val_idx >= values_len {
                    return return_pack_result(ctx, Vec::new(), "missing value for pack format");
                }
                let elem_root = ctx.list_get_rooted(&values_root, val_idx)?;
                val_idx += 1;
                let elem = ctx.reload_root(&elem_root)?;
                let Ok(num) = ctx.number_of(elem) else {
                    return return_pack_result(ctx, Vec::new(), "expected number for 'd'");
                };
                let v = num.to_f64();
                let bytes = match hdr.endian {
                    Endian::Little => v.to_le_bytes(),
                    Endian::Big => v.to_be_bytes(),
                };
                if out.len().saturating_add(8) > u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                out.extend_from_slice(&bytes);
            }
            PackOp::FixedBytes(sz) => {
                if val_idx >= values_len {
                    return return_pack_result(ctx, Vec::new(), "missing value for pack format");
                }
                let elem_root = ctx.list_get_rooted(&values_root, val_idx)?;
                val_idx += 1;
                let elem = ctx.reload_root(&elem_root)?;
                let Some(s) = extract_string_elem(elem) else {
                    return return_pack_result(ctx, Vec::new(), "expected String for 'c'");
                };
                if s.len() > sz {
                    return return_pack_result(ctx, Vec::new(), "string longer than given size");
                }
                if out.len().saturating_add(sz) > u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                out.extend_from_slice(s.as_bytes());
                out.resize(out.len() + (sz - s.len()), 0);
            }
            PackOp::ZeroTermStr => {
                if val_idx >= values_len {
                    return return_pack_result(ctx, Vec::new(), "missing value for pack format");
                }
                let elem_root = ctx.list_get_rooted(&values_root, val_idx)?;
                val_idx += 1;
                let elem = ctx.reload_root(&elem_root)?;
                let Some(s) = extract_string_elem(elem) else {
                    return return_pack_result(ctx, Vec::new(), "expected String for 'z'");
                };
                if s.as_bytes().contains(&0) {
                    return return_pack_result(ctx, Vec::new(), "string contains zeros");
                }
                if out.len().saturating_add(s.len() + 1) > u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            PackOp::SizedStr(sz) => {
                if val_idx >= values_len {
                    return return_pack_result(ctx, Vec::new(), "missing value for pack format");
                }
                let elem_root = ctx.list_get_rooted(&values_root, val_idx)?;
                val_idx += 1;
                let elem = ctx.reload_root(&elem_root)?;
                let Some(s) = extract_string_elem(elem) else {
                    return return_pack_result(ctx, Vec::new(), "expected String for 's'");
                };
                let slen = s.len() as u128;
                if sz < 16 && slen >= (1u128 << (sz * 8)) {
                    return return_pack_result(
                        ctx,
                        Vec::new(),
                        "string length does not fit in given size",
                    );
                }
                if out.len().saturating_add(sz).saturating_add(s.len()) > u16::MAX as usize {
                    return Err(VmError::ObjectTooLarge);
                }
                write_uint_bytes(&mut out, slen, sz, hdr.endian);
                out.extend_from_slice(s.as_bytes());
            }
        }
    }

    return_pack_result(ctx, out, "")
}

fn return_pack_result(ctx: &mut BuiltinCtx<'_>, bytes: Vec<u8>, err: &str) -> Result<(), VmError> {
    let items = bytes
        .into_iter()
        .map(|b| NativeValue::I64(i64::from(b)))
        .collect();
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::List(items),
        NativeValue::Str(err.to_string()),
    ]))
}

/// `583` `__bytes_packsize(fmt: String) -> (i64, String)`
pub fn bytes_packsize(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let fmt_str = ctx.arg_string(0)?;
    let fmt = fmt_str.as_bytes();
    let mut i = 0usize;
    let mut hdr = PackHeader::default();
    let mut total = 0usize;

    while let Some(op) = match next_pack_op(fmt, &mut i, &mut hdr) {
        Ok(op) => op,
        Err(msg) => {
            return ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(-1),
                NativeValue::Str(msg),
            ]));
        }
    } {
        if matches!(op, PackOp::Noop) {
            continue;
        }
        if matches!(op, PackOp::ZeroTermStr | PackOp::SizedStr(_)) {
            return ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(-1),
                NativeValue::Str("variable-length format".to_string()),
            ]));
        }
        let pad = match compute_padding(total, op, hdr.max_align) {
            Ok(p) => p,
            Err(msg) => {
                return ctx.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(-1),
                    NativeValue::Str(msg),
                ]));
            }
        };
        let item_sz = match op {
            PackOp::SignedInt(sz) | PackOp::UnsignedInt { size: sz, .. } => sz,
            PackOp::F32 => 4,
            PackOp::F64 => 8,
            PackOp::FixedBytes(sz) => sz,
            PackOp::PaddingByte => 1,
            PackOp::AlignTo(_) | PackOp::Noop => 0,
            PackOp::ZeroTermStr | PackOp::SizedStr(_) => unreachable!(),
        };
        let Some(next_total) = total.checked_add(pad).and_then(|v| v.checked_add(item_sz)) else {
            return ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(-1),
                NativeValue::Str("format result too large".to_string()),
            ]));
        };
        if next_total > i32::MAX as usize {
            return ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(-1),
                NativeValue::Str("format result too large".to_string()),
            ]));
        }
        total = next_total;
    }

    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::I64(total as i64),
        NativeValue::Str(String::new()),
    ]))
}

/// `584` `__bytes_unpack(fmt: String, bytes: List, pos: i64) -> (List, i64, String)`
pub fn bytes_unpack(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let fmt_str = ctx.arg_string(0)?;
    let bytes_root = ctx.arg_rooted(1)?;
    let init_pos = ctx.arg_i64(2)?;

    let bytes_val = ctx.reload_root(&bytes_root)?;
    let bytes_len = ctx.list_len(bytes_val)?;
    let mut buf = Vec::with_capacity(bytes_len);
    for idx in 0..bytes_len {
        let elem_root = ctx.list_get_rooted(&bytes_root, idx)?;
        let elem = ctx.reload_root(&elem_root)?;
        let num = ctx.number_of(elem)?;
        let b = match num {
            Number::I64(v) => u8::try_from(v).ok(),
            Number::U64(v) => u8::try_from(v).ok(),
            Number::I128(v) => u8::try_from(v).ok(),
            Number::U128(v) => u8::try_from(v).ok(),
            Number::F64(_) => return Err(VmError::TypeError),
        };
        let Some(byte) = b else {
            return return_unpack_result(
                ctx,
                Vec::new(),
                init_pos,
                "byte value out of range 0..=255",
            );
        };
        buf.push(byte);
    }

    let resolved = if init_pos < 0 {
        (buf.len() as i64).checked_add(init_pos)
    } else {
        Some(init_pos)
    };
    let Some(pos_i64) = resolved else {
        return return_unpack_result(ctx, Vec::new(), init_pos, "initial position out of string");
    };
    if pos_i64 < 0 || (pos_i64 as usize) > buf.len() {
        return return_unpack_result(ctx, Vec::new(), init_pos, "initial position out of string");
    }

    let base_pos = pos_i64 as usize;
    let mut cur = base_pos;
    let fmt = fmt_str.as_bytes();
    let mut i = 0usize;
    let mut hdr = PackHeader::default();
    let mut values: Vec<NativeValue> = Vec::new();

    while let Some(op) = match next_pack_op(fmt, &mut i, &mut hdr) {
        Ok(op) => op,
        Err(msg) => return return_unpack_result(ctx, Vec::new(), base_pos as i64, &msg),
    } {
        if matches!(op, PackOp::Noop) {
            continue;
        }
        // Alignment in Lua `string.unpack` is computed relative to the start of `bytes` (`cur`)!
        let pad = match compute_padding(cur, op, hdr.max_align) {
            Ok(p) => p,
            Err(msg) => return return_unpack_result(ctx, Vec::new(), base_pos as i64, &msg),
        };
        if cur.saturating_add(pad) > buf.len() {
            return return_unpack_result(ctx, Vec::new(), base_pos as i64, "data string too short");
        }
        cur += pad;

        match op {
            PackOp::Noop | PackOp::AlignTo(_) => {}
            PackOp::PaddingByte => {
                if cur + 1 > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                cur += 1;
            }
            PackOp::SignedInt(sz) => {
                if cur + sz > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                let val = read_signed_bytes(&buf[cur..cur + sz], hdr.endian);
                let Ok(v64) = i64::try_from(val) else {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        &format!("{sz}-byte integer does not fit into i64"),
                    );
                };
                cur += sz;
                values.push(NativeValue::I64(v64));
            }
            PackOp::UnsignedInt {
                size: sz,
                return_u64,
            } => {
                if cur + sz > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                let val = read_uint_bytes(&buf[cur..cur + sz], hdr.endian);
                cur += sz;
                if return_u64 {
                    let Ok(u) = u64::try_from(val) else {
                        return return_unpack_result(
                            ctx,
                            Vec::new(),
                            base_pos as i64,
                            &format!("{sz}-byte unsigned integer does not fit into u64"),
                        );
                    };
                    values.push(NativeValue::U64(u));
                } else {
                    values.push(NativeValue::I64(val as i64));
                }
            }
            PackOp::F32 => {
                if cur + 4 > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                let mut arr = [0u8; 4];
                arr.copy_from_slice(&buf[cur..cur + 4]);
                let v = match hdr.endian {
                    Endian::Little => f32::from_le_bytes(arr),
                    Endian::Big => f32::from_be_bytes(arr),
                };
                cur += 4;
                values.push(NativeValue::F64(f64::from(v)));
            }
            PackOp::F64 => {
                if cur + 8 > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&buf[cur..cur + 8]);
                let v = match hdr.endian {
                    Endian::Little => f64::from_le_bytes(arr),
                    Endian::Big => f64::from_be_bytes(arr),
                };
                cur += 8;
                values.push(NativeValue::F64(v));
            }
            PackOp::FixedBytes(sz) => {
                if cur + sz > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                let s = bytes_slice_to_string(&buf[cur..cur + sz]);
                ctx.check_string_size(s.len())?;
                cur += sz;
                values.push(NativeValue::Str(s));
            }
            PackOp::ZeroTermStr => {
                let Some(rel_nul) = buf[cur..].iter().position(|&b| b == 0) else {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "unfinished string for format 'z'",
                    );
                };
                let s = bytes_slice_to_string(&buf[cur..cur + rel_nul]);
                ctx.check_string_size(s.len())?;
                cur += rel_nul + 1;
                values.push(NativeValue::Str(s));
            }
            PackOp::SizedStr(sz) => {
                if cur + sz > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                let raw_len = read_uint_bytes(&buf[cur..cur + sz], hdr.endian);
                let Ok(slen) = usize::try_from(raw_len) else {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                };
                cur += sz;
                if cur.saturating_add(slen) > buf.len() {
                    return return_unpack_result(
                        ctx,
                        Vec::new(),
                        base_pos as i64,
                        "data string too short",
                    );
                }
                let s = bytes_slice_to_string(&buf[cur..cur + slen]);
                ctx.check_string_size(s.len())?;
                cur += slen;
                values.push(NativeValue::Str(s));
            }
        }
    }

    return_unpack_result(ctx, values, cur as i64, "")
}

fn return_unpack_result(
    ctx: &mut BuiltinCtx<'_>,
    values: Vec<NativeValue>,
    next_pos: i64,
    err: &str,
) -> Result<(), VmError> {
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::List(values),
        NativeValue::I64(next_pos),
        NativeValue::Str(err.to_string()),
    ]))
}

fn write_uint_bytes(out: &mut Vec<u8>, val: u128, sz: usize, endian: Endian) {
    match endian {
        Endian::Little => {
            let le = val.to_le_bytes();
            out.extend_from_slice(&le[..sz]);
        }
        Endian::Big => {
            let be = val.to_be_bytes();
            out.extend_from_slice(&be[16 - sz..]);
        }
    }
}

fn read_uint_bytes(slice: &[u8], endian: Endian) -> u128 {
    let mut acc = 0u128;
    match endian {
        Endian::Little => {
            for (idx, &b) in slice.iter().enumerate() {
                acc |= u128::from(b) << (idx * 8);
            }
        }
        Endian::Big => {
            for &b in slice {
                acc = (acc << 8) | u128::from(b);
            }
        }
    }
    acc
}

fn read_signed_bytes(slice: &[u8], endian: Endian) -> i128 {
    let sz = slice.len();
    let raw = read_uint_bytes(slice, endian);
    if sz == 16 {
        return raw as i128;
    }
    let sign_bit = 1u128 << (sz * 8 - 1);
    if (raw & sign_bit) != 0 {
        let mask = !((1u128 << (sz * 8)) - 1);
        (raw | mask) as i128
    } else {
        raw as i128
    }
}

fn bytes_slice_to_string(slice: &[u8]) -> String {
    match std::str::from_utf8(slice) {
        Ok(s) => s.to_owned(),
        Err(_) => String::from_utf8_lossy(slice).into_owned(),
    }
}

fn extract_string_elem(val: runtime::TaggedValue) -> Option<String> {
    let ptr = val.as_heap_ptr()?;
    // SAFETY: `val` was loaded from a live `BuiltinRoot` in an active `BuiltinCtx`.
    let header = unsafe { gc::ObjectHeader::from_payload_ptr(ptr) };
    if !header.is_ordinary()
        || header.type_index != type_pool::Intrinsic::Str.type_index()
        || header.payload_words() == 0
    {
        return None;
    }
    // SAFETY: checked ordinary `Str` header with a valid length prefix word.
    let len = unsafe { *(ptr as *const u64) } as usize;
    if len > header.payload_words() * 8 - 8 {
        return None;
    }
    // SAFETY: `len` lies within the string's payload.
    let bytes = unsafe { std::slice::from_raw_parts(ptr.add(8), len) };
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

// ============================================================================
// Compact GC No-Scan `Bytes` Builtins (`648..=663`, `669`)
// ============================================================================

pub fn bytes_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let argc = ctx.arg_count();
    let (cap, init_len) = match argc {
        1 => (ctx.arg_usize(0)?, 0),
        2 => (ctx.arg_usize(0)?, ctx.arg_usize(1)?),
        _ => {
            return Err(VmError::ArityError {
                expected: 1,
                got: argc,
            });
        }
    };
    ctx.return_new_bytes(cap, init_len)
}

pub fn bytes_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let b = ctx.arg_rooted(0)?;
    let len = ctx.bytes_len(ctx.load_rooted(&b)?)?;
    ctx.return_usize(len)
}

pub fn bytes_cap(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let b = ctx.arg_rooted(0)?;
    let cap = ctx.bytes_cap(ctx.load_rooted(&b)?)?;
    ctx.return_usize(cap)
}

pub fn bytes_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let b = ctx.arg_rooted(0)?;
    let idx = ctx.arg_usize(1)?;
    let byte = ctx.bytes_get(ctx.load_rooted(&b)?, idx)?;
    ctx.return_u8(byte)
}

pub fn bytes_set(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let b = ctx.arg_rooted(0)?;
    let idx = ctx.arg_usize(1)?;
    let val = ctx.arg_byte(2)?;
    let raw = ctx.load_rooted(&b)?;
    ctx.bytes_set(raw, idx, val)?;
    ctx.return_unit();
    Ok(())
}

pub fn bytes_push(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let b = ctx.arg_rooted(0)?;
    let val = ctx.arg_byte(1)?;
    let raw = ctx.load_rooted(&b)?;
    ctx.bytes_push(raw, val)?;
    ctx.return_unit();
    Ok(())
}

pub fn bytes_clear(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let b = ctx.arg_rooted(0)?;
    let raw = ctx.load_rooted(&b)?;
    ctx.bytes_clear(raw)?;
    ctx.return_unit();
    Ok(())
}

pub fn bytes_slice(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let b = ctx.arg_rooted(0)?;
    let start = ctx.arg_usize(1)?;
    let end = ctx.arg_usize(2)?;
    let raw = ctx.load_rooted(&b)?;
    ctx.return_bytes_slice(raw, start, end)
}

pub fn bytes_copy(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let b = ctx.arg_rooted(0)?;
    let raw = ctx.load_rooted(&b)?;
    ctx.return_bytes_copy(raw)
}

pub fn bytes_extend(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let dst = ctx.arg_rooted(0)?;
    let src = ctx.arg_rooted(1)?;
    let dst_raw = ctx.load_rooted(&dst)?;
    let src_raw = ctx.load_rooted(&src)?;
    ctx.bytes_extend(dst_raw, src_raw)?;
    ctx.return_unit();
    Ok(())
}

pub fn bytes_from_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let s = ctx.arg_rooted(0)?;
    let raw = ctx.load_rooted(&s)?;
    ctx.return_bytes_from_str(raw)
}

pub fn bytes_to_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let b = ctx.arg_rooted(0)?;
    let raw = ctx.load_rooted(&b)?;
    ctx.return_bytes_to_str(raw)
}

pub fn bytes_from_list(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg_rooted(0)?;
    let raw = ctx.load_rooted(&list)?;
    ctx.return_bytes_from_list(raw)
}

pub fn bytes_to_list(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let b = ctx.arg_rooted(0)?;
    let raw = ctx.load_rooted(&b)?;
    ctx.return_bytes_to_list(raw)
}

pub fn bytes_find(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let argc = ctx.arg_count();
    let start = match argc {
        2 => 0,
        3 => ctx.arg_usize(2)?,
        _ => {
            return Err(VmError::ArityError {
                expected: 3,
                got: argc,
            });
        }
    };
    let hay = ctx.arg_rooted(0)?;
    let needle = ctx.arg_rooted(1)?;
    let hay_raw = ctx.load_rooted(&hay)?;
    let needle_raw = ctx.load_rooted(&needle)?;
    match ctx.bytes_find(hay_raw, needle_raw, start)? {
        Some(pos) => ctx.return_usize(pos),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn bytes_eq(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg_rooted(0)?;
    let right = ctx.arg_rooted(1)?;
    let eq = ctx.bytes_eq(ctx.load_rooted(&left)?, ctx.load_rooted(&right)?)?;
    ctx.return_bool(eq);
    Ok(())
}

pub fn bytes_read_u64_le(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let b = ctx.arg_rooted(0)?;
    let offset = ctx.arg_usize(1)?;
    let word = ctx.bytes_read_u64_le(ctx.load_rooted(&b)?, offset)?;
    ctx.return_u64(word)
}
