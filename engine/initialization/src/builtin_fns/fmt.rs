//! Scalar hashing, `printf`/`string.format` formatting, and IEEE-754 float helpers (`573..=579`).

use interpreter::resources::ldexp_f64;
use interpreter::{BuiltinCtx, NativeValue, VmError};
use runtime::{Number, TaggedValue};

const FMT_OK: i64 = 0;
const FMT_ERR_INVALID_SPEC: i64 = 1;
const FMT_ERR_MISSING_ARG: i64 = 2;
const FMT_ERR_TYPE_MISMATCH: i64 = 3;
const FMT_ERR_OUT_OF_RANGE: i64 = 4;

const MAX_FIELD_WIDTH: usize = 10_000;
const MAX_PRECISION: usize = 10_000;

/// `573` `__scalar_hash(val: Any) -> u64`
pub fn scalar_hash(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let root = ctx.arg_rooted(0)?;
    let val = ctx.reload_root(&root)?;
    let h = ctx.scalar_hash(val)?;
    ctx.return_number(Number::U64(h))
}

/// `574` `__fmt_sprintf(spec: String, args: List) -> (i64, String)`
pub fn fmt_sprintf(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let spec = ctx.arg_string(0)?;
    let args_root = ctx.arg_rooted(1)?;
    let args_val = ctx.reload_root(&args_root)?;
    let args_len = ctx.list_len(args_val)?;

    let mut out = String::with_capacity(spec.len());
    let bytes = spec.as_bytes();
    let mut i = 0usize;
    let mut arg_idx = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'%' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'%' {
                i += 1;
            }
            let literal = &spec[start..i];
            let next_len = out
                .len()
                .checked_add(literal.len())
                .ok_or(VmError::DisplaySizeExceeded)?;
            ctx.check_string_size(next_len)?;
            out.push_str(literal);
            continue;
        }

        // Consume '%'
        i += 1;
        if i >= bytes.len() {
            return return_fmt_result(ctx, FMT_ERR_INVALID_SPEC, "incomplete format specifier '%'");
        }
        if bytes[i] == b'%' {
            i += 1;
            let next_len = out
                .len()
                .checked_add(1)
                .ok_or(VmError::DisplaySizeExceeded)?;
            ctx.check_string_size(next_len)?;
            out.push('%');
            continue;
        }

        let mut flags = FmtFlags::default();
        while i < bytes.len() {
            match bytes[i] {
                b'-' => flags.left_align = true,
                b'+' => flags.plus = true,
                b' ' => flags.space = true,
                b'#' => flags.alt = true,
                b'0' => flags.zero_pad = true,
                _ => break,
            }
            i += 1;
        }

        let mut width: Option<usize> = None;
        if i < bytes.len() && bytes[i].is_ascii_digit() {
            let mut w = 0usize;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                w = w
                    .saturating_mul(10)
                    .saturating_add(usize::from(bytes[i] - b'0'));
                i += 1;
            }
            if w > MAX_FIELD_WIDTH {
                return return_fmt_result(
                    ctx,
                    FMT_ERR_INVALID_SPEC,
                    "format field width exceeds maximum limit",
                );
            }
            width = Some(w);
        }

        let mut precision: Option<usize> = None;
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            let mut p = 0usize;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                p = p
                    .saturating_mul(10)
                    .saturating_add(usize::from(bytes[i] - b'0'));
                i += 1;
            }
            if p > MAX_PRECISION {
                return return_fmt_result(
                    ctx,
                    FMT_ERR_INVALID_SPEC,
                    "format precision exceeds maximum limit",
                );
            }
            precision = Some(p);
        }

        if i >= bytes.len() {
            return return_fmt_result(ctx, FMT_ERR_INVALID_SPEC, "unfinished format specifier");
        }
        let conv = bytes[i];
        // Advance past the conversion character (ensuring UTF-8 boundary if non-ASCII).
        if conv.is_ascii() {
            i += 1;
        } else {
            return return_fmt_result(
                ctx,
                FMT_ERR_INVALID_SPEC,
                "invalid non-ASCII conversion specifier",
            );
        }

        if !matches!(
            conv,
            b'd' | b'i'
                | b'u'
                | b'o'
                | b'x'
                | b'X'
                | b'b'
                | b'f'
                | b'F'
                | b'e'
                | b'E'
                | b'g'
                | b'G'
                | b'a'
                | b'A'
                | b'c'
                | b's'
                | b'q'
                | b'p'
        ) {
            return return_fmt_result(
                ctx,
                FMT_ERR_INVALID_SPEC,
                format!("invalid conversion specifier '%{}'", conv as char),
            );
        }

        if conv == b'q' && (flags.any() || width.is_some() || precision.is_some()) {
            return return_fmt_result(
                ctx,
                FMT_ERR_INVALID_SPEC,
                "specifier '%q' cannot have modifiers",
            );
        }

        if arg_idx >= args_len {
            return return_fmt_result(
                ctx,
                FMT_ERR_MISSING_ARG,
                format!("missing argument #{} for '%{}'", arg_idx + 1, conv as char),
            );
        }
        let elem_root = ctx.list_get_rooted(&args_root, arg_idx)?;
        arg_idx += 1;
        let elem_val = ctx.reload_root(&elem_root)?;

        let piece = match format_one_directive(ctx, conv, flags, width, precision, elem_val)? {
            Ok(rendered) => rendered,
            Err((code, msg)) => return return_fmt_result(ctx, code, msg),
        };

        let next_len = out
            .len()
            .checked_add(piece.len())
            .ok_or(VmError::DisplaySizeExceeded)?;
        ctx.check_string_size(next_len)?;
        out.push_str(&piece);
    }

    return_fmt_result(ctx, FMT_OK, out)
}

fn return_fmt_result(
    ctx: &mut BuiltinCtx<'_>,
    code: i64,
    text: impl Into<String>,
) -> Result<(), VmError> {
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::I64(code),
        NativeValue::Str(text.into()),
    ]))
}

#[derive(Default, Clone, Copy)]
struct FmtFlags {
    left_align: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero_pad: bool,
}

impl FmtFlags {
    fn any(self) -> bool {
        self.left_align || self.plus || self.space || self.alt || self.zero_pad
    }
}

fn format_one_directive(
    ctx: &BuiltinCtx<'_>,
    conv: u8,
    flags: FmtFlags,
    width: Option<usize>,
    precision: Option<usize>,
    val: TaggedValue,
) -> Result<Result<String, (i64, String)>, VmError> {
    match conv {
        b'd' | b'i' => {
            let Ok(num) = ctx.number_of(val) else {
                return Ok(Err((
                    FMT_ERR_TYPE_MISMATCH,
                    format!("expected integer for '%{}'", conv as char),
                )));
            };
            let signed: i128 = match num {
                Number::I64(v) => i128::from(v),
                Number::U64(v) => i128::from(v),
                Number::I128(v) => v,
                Number::U128(v) => match i128::try_from(v) {
                    Ok(s) => s,
                    Err(_) => {
                        return Ok(Err((
                            FMT_ERR_OUT_OF_RANGE,
                            "integer out of range for '%d'".to_string(),
                        )));
                    }
                },
                Number::F64(_) => {
                    return Ok(Err((
                        FMT_ERR_TYPE_MISMATCH,
                        format!("expected integer for '%{}', got float", conv as char),
                    )));
                }
            };
            let is_neg = signed < 0;
            let mag = signed.unsigned_abs();
            let sign_str = if is_neg {
                "-"
            } else if flags.plus {
                "+"
            } else if flags.space {
                " "
            } else {
                ""
            };
            let digits = if mag == 0 && precision == Some(0) {
                String::new()
            } else {
                mag.to_string()
            };
            Ok(Ok(pad_integer(
                sign_str, "", &digits, flags, width, precision,
            )))
        }
        b'u' | b'o' | b'x' | b'X' | b'b' => {
            let Ok(num) = ctx.number_of(val) else {
                return Ok(Err((
                    FMT_ERR_TYPE_MISMATCH,
                    format!("expected integer for '%{}'", conv as char),
                )));
            };
            let unsigned: u128 = match num {
                // Match C/Lua 64-bit two's-complement wraparound for signed 64-bit inputs.
                Number::I64(v) => u128::from(v as u64),
                Number::U64(v) => u128::from(v),
                Number::I128(v) => {
                    if let Ok(v64) = i64::try_from(v) {
                        u128::from(v64 as u64)
                    } else {
                        v as u128
                    }
                }
                Number::U128(v) => v,
                Number::F64(_) => {
                    return Ok(Err((
                        FMT_ERR_TYPE_MISMATCH,
                        format!("expected integer for '%{}', got float", conv as char),
                    )));
                }
            };
            let mut digits = if unsigned == 0 && precision == Some(0) {
                String::new()
            } else {
                match conv {
                    b'u' => unsigned.to_string(),
                    b'o' => format!("{unsigned:o}"),
                    b'x' => format!("{unsigned:x}"),
                    b'X' => format!("{unsigned:X}"),
                    b'b' => format!("{unsigned:b}"),
                    _ => unreachable!(),
                }
            };
            let prefix = if flags.alt && unsigned != 0 {
                match conv {
                    b'o' => {
                        if !digits.starts_with('0') {
                            digits.insert(0, '0');
                        }
                        ""
                    }
                    b'x' => "0x",
                    b'X' => "0X",
                    b'b' => "0b",
                    _ => "",
                }
            } else if flags.alt && conv == b'o' && digits.is_empty() {
                "0"
            } else {
                ""
            };
            Ok(Ok(pad_integer(
                "", prefix, &digits, flags, width, precision,
            )))
        }
        b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
            let Ok(num) = ctx.number_of(val) else {
                return Ok(Err((
                    FMT_ERR_TYPE_MISMATCH,
                    format!("expected number for '%{}'", conv as char),
                )));
            };
            let x = num.to_f64();
            let upper = conv.is_ascii_uppercase();
            Ok(Ok(format_float_directive(
                x, conv, upper, flags, width, precision,
            )))
        }
        b'c' => {
            let ch = if let Some(c) = val.as_char() {
                c
            } else if let Ok(num) = ctx.number_of(val) {
                let code = match num {
                    Number::I64(v) => u32::try_from(v).ok(),
                    Number::U64(v) => u32::try_from(v).ok(),
                    Number::I128(v) => u32::try_from(v).ok(),
                    Number::U128(v) => u32::try_from(v).ok(),
                    Number::F64(_) => {
                        return Ok(Err((
                            FMT_ERR_TYPE_MISMATCH,
                            "expected char or integer for '%c'".to_string(),
                        )));
                    }
                };
                match code.and_then(char::from_u32) {
                    Some(c) => c,
                    None => {
                        return Ok(Err((
                            FMT_ERR_OUT_OF_RANGE,
                            "character code out of valid Unicode scalar range".to_string(),
                        )));
                    }
                }
            } else {
                return Ok(Err((
                    FMT_ERR_TYPE_MISMATCH,
                    "expected char or integer for '%c'".to_string(),
                )));
            };
            let mut s = String::new();
            s.push(ch);
            Ok(Ok(pad_text(&s, flags.left_align, false, width)))
        }
        b's' => {
            let mut text = ctx.format_value(val)?;
            if let Some(max_bytes) = precision
                && max_bytes < text.len()
            {
                let mut end = max_bytes;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            let zero_fill = flags.zero_pad && !flags.left_align;
            Ok(Ok(pad_text(&text, flags.left_align, zero_fill, width)))
        }
        b'q' => quote_literal_value(val),
        b'p' => {
            if flags.plus || flags.space || flags.alt || flags.zero_pad || precision.is_some() {
                return Ok(Err((
                    FMT_ERR_INVALID_SPEC,
                    "invalid modifier for '%p'".to_string(),
                )));
            }
            let rendered = if ctx.number_of(val).is_err()
                && let Some(ptr) = val.as_heap_ptr()
            {
                format!("0x{:x}", ptr as usize)
            } else {
                "(null)".to_string()
            };
            Ok(Ok(pad_text(&rendered, flags.left_align, false, width)))
        }
        _ => Ok(Err((
            FMT_ERR_INVALID_SPEC,
            "invalid format specifier".to_string(),
        ))),
    }
}

fn pad_integer(
    sign: &str,
    prefix: &str,
    digits: &str,
    flags: FmtFlags,
    width: Option<usize>,
    precision: Option<usize>,
) -> String {
    let prec_zeros = precision.unwrap_or(0).saturating_sub(digits.len());
    let core_len = sign.len() + prefix.len() + prec_zeros + digits.len();
    let target_w = width.unwrap_or(0);
    let pad_len = target_w.saturating_sub(core_len);

    // Per C99 / Lua printf rules, explicit precision disables the '0' flag on integer conversions.
    let use_zero_pad = flags.zero_pad && !flags.left_align && precision.is_none();

    let mut out = String::with_capacity(core_len + pad_len);
    if !flags.left_align && !use_zero_pad {
        for _ in 0..pad_len {
            out.push(' ');
        }
    }
    out.push_str(sign);
    out.push_str(prefix);
    if use_zero_pad {
        for _ in 0..pad_len {
            out.push('0');
        }
    }
    for _ in 0..prec_zeros {
        out.push('0');
    }
    out.push_str(digits);
    if flags.left_align {
        for _ in 0..pad_len {
            out.push(' ');
        }
    }
    out
}

fn pad_text(text: &str, left_align: bool, zero_pad: bool, width: Option<usize>) -> String {
    let target_w = width.unwrap_or(0);
    let pad_len = target_w.saturating_sub(text.len());
    if pad_len == 0 {
        return text.to_string();
    }
    let fill = if zero_pad && !left_align { '0' } else { ' ' };
    let mut out = String::with_capacity(text.len() + pad_len);
    if !left_align {
        for _ in 0..pad_len {
            out.push(fill);
        }
    }
    out.push_str(text);
    if left_align {
        for _ in 0..pad_len {
            out.push(' ');
        }
    }
    out
}

fn format_float_directive(
    x: f64,
    conv: u8,
    upper: bool,
    flags: FmtFlags,
    width: Option<usize>,
    precision: Option<usize>,
) -> String {
    if x.is_nan() {
        let sign = if flags.plus {
            "+"
        } else if flags.space {
            " "
        } else {
            ""
        };
        let body = if upper { "NAN" } else { "nan" };
        let combined = format!("{sign}{body}");
        return pad_text(&combined, flags.left_align, false, width);
    }
    if x.is_infinite() {
        let sign = if x.is_sign_negative() {
            "-"
        } else if flags.plus {
            "+"
        } else if flags.space {
            " "
        } else {
            ""
        };
        let body = if upper { "INF" } else { "inf" };
        let combined = format!("{sign}{body}");
        return pad_text(&combined, flags.left_align, false, width);
    }

    let is_neg = x.is_sign_negative();
    let sign = if is_neg {
        "-"
    } else if flags.plus {
        "+"
    } else if flags.space {
        " "
    } else {
        ""
    };
    let abs_x = x.abs();

    let (prefix, body) = match conv {
        b'a' | b'A' => {
            let pfx = if upper { "0X" } else { "0x" };
            (
                pfx,
                format_hex_float_body(abs_x, upper, precision, flags.alt),
            )
        }
        b'f' | b'F' => {
            let prec = precision.unwrap_or(6);
            let mut s = format!("{abs_x:.prec$}");
            if prec == 0 && flags.alt && !s.contains('.') {
                s.push('.');
            }
            ("", s)
        }
        b'e' | b'E' => {
            let prec = precision.unwrap_or(6);
            (
                "",
                format_decimal_exp_body(abs_x, prec, upper, flags.alt, 2),
            )
        }
        b'g' | b'G' => {
            let sig = precision.unwrap_or(6).max(1);
            ("", format_general_float_body(abs_x, sig, upper, flags.alt))
        }
        _ => unreachable!(),
    };

    let core_len = sign.len() + prefix.len() + body.len();
    let pad_len = width.unwrap_or(0).saturating_sub(core_len);
    let use_zero = flags.zero_pad && !flags.left_align;

    let mut out = String::with_capacity(core_len + pad_len);
    if !flags.left_align && !use_zero {
        for _ in 0..pad_len {
            out.push(' ');
        }
    }
    out.push_str(sign);
    out.push_str(prefix);
    if use_zero {
        for _ in 0..pad_len {
            out.push('0');
        }
    }
    out.push_str(&body);
    if flags.left_align {
        for _ in 0..pad_len {
            out.push(' ');
        }
    }
    out
}

fn format_decimal_exp_body(
    abs_x: f64,
    prec: usize,
    upper: bool,
    alt: bool,
    min_exp_digits: usize,
) -> String {
    let raw = format!("{abs_x:.prec$e}");
    let (mant, exp_str) = raw.split_once('e').expect("scientific format contains 'e'");
    let exp_val: i32 = exp_str.parse().unwrap_or(0);
    let mut out = String::with_capacity(raw.len() + 4);
    out.push_str(mant);
    if prec == 0 && alt && !mant.contains('.') {
        out.push('.');
    }
    out.push(if upper { 'E' } else { 'e' });
    if exp_val < 0 {
        out.push('-');
    } else {
        out.push('+');
    }
    let abs_exp = exp_val.unsigned_abs().to_string();
    for _ in 0..min_exp_digits.saturating_sub(abs_exp.len()) {
        out.push('0');
    }
    out.push_str(&abs_exp);
    out
}

fn format_general_float_body(abs_x: f64, sig_digits: usize, upper: bool, alt: bool) -> String {
    let raw = format!("{abs_x:.prec$e}", prec = sig_digits - 1);
    let (mant, exp_str) = raw.split_once('e').expect("scientific format contains 'e'");
    let exp_val: i32 = exp_str.parse().unwrap_or(0);

    if exp_val < -4 || exp_val >= sig_digits as i32 {
        let mut trimmed_mant = mant.to_string();
        if !alt && trimmed_mant.contains('.') {
            while trimmed_mant.ends_with('0') {
                trimmed_mant.pop();
            }
            if trimmed_mant.ends_with('.') {
                trimmed_mant.pop();
            }
        } else if alt && !trimmed_mant.contains('.') {
            trimmed_mant.push('.');
        }
        let exp_ch = if upper { 'E' } else { 'e' };
        let sign_ch = if exp_val < 0 { '-' } else { '+' };
        let abs_exp = exp_val.unsigned_abs();
        format!("{trimmed_mant}{exp_ch}{sign_ch}{abs_exp:02}")
    } else {
        let frac_places = (sig_digits as i32 - 1 - exp_val).max(0) as usize;
        let mut fixed = format!("{abs_x:.frac_places$}");
        if !alt && fixed.contains('.') {
            while fixed.ends_with('0') {
                fixed.pop();
            }
            if fixed.ends_with('.') {
                fixed.pop();
            }
        } else if alt && !fixed.contains('.') {
            fixed.push('.');
        }
        fixed
    }
}

fn format_hex_float_body(abs_x: f64, upper: bool, precision: Option<usize>, alt: bool) -> String {
    let exp_ch = if upper { 'P' } else { 'p' };
    if abs_x == 0.0 {
        let mut out = String::from("0");
        match precision {
            Some(0) => {
                if alt {
                    out.push('.');
                }
            }
            Some(p) => {
                out.push('.');
                for _ in 0..p {
                    out.push('0');
                }
            }
            None => {
                if alt {
                    out.push('.');
                }
            }
        }
        out.push(exp_ch);
        out.push_str("+0");
        return out;
    }

    let bits = abs_x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let mut frac = bits & 0x000f_ffff_ffff_ffff;
    let (mut lead, exp2) = if biased == 0 {
        let mut e = -1022i32;
        while (frac & (1u64 << 52)) == 0 {
            frac <<= 1;
            e -= 1;
        }
        frac &= 0x000f_ffff_ffff_ffff;
        (1u64, e)
    } else {
        (1u64, biased - 1023)
    };

    let hex_digits = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };

    let mut out = String::new();
    match precision {
        None => {
            let mut nibbles = [0u8; 13];
            for (idx, slot) in nibbles.iter_mut().enumerate() {
                let shift = (12 - idx) * 4;
                *slot = hex_digits[((frac >> shift) & 0xf) as usize];
            }
            let mut len = 13usize;
            while len > 0 && nibbles[len - 1] == b'0' {
                len -= 1;
            }
            out.push(hex_digits[lead as usize] as char);
            if len > 0 || alt {
                out.push('.');
                for &b in &nibbles[..len] {
                    out.push(b as char);
                }
            }
        }
        Some(0) => {
            let half = 1u64 << 51;
            if frac > half || (frac == half && (lead & 1) == 1) {
                lead += 1;
            }
            out.push(hex_digits[lead as usize] as char);
            if alt {
                out.push('.');
            }
        }
        Some(p) if p < 13 => {
            let drop_bits = (13 - p) * 4;
            let half = 1u64 << (drop_bits - 1);
            let rem = frac & ((1u64 << drop_bits) - 1);
            let mut kept = frac >> drop_bits;
            if rem > half || (rem == half && (kept & 1) == 1) {
                kept += 1;
                if kept >= (1u64 << (p * 4)) {
                    lead += 1;
                    kept = 0;
                }
            }
            out.push(hex_digits[lead as usize] as char);
            out.push('.');
            for idx in 0..p {
                let shift = (p - 1 - idx) * 4;
                out.push(hex_digits[((kept >> shift) & 0xf) as usize] as char);
            }
        }
        Some(p) => {
            out.push(hex_digits[lead as usize] as char);
            out.push('.');
            for idx in 0..13 {
                let shift = (12 - idx) * 4;
                out.push(hex_digits[((frac >> shift) & 0xf) as usize] as char);
            }
            for _ in 13..p {
                out.push('0');
            }
        }
    }

    out.push(exp_ch);
    if exp2 >= 0 {
        out.push('+');
        out.push_str(&exp2.to_string());
    } else {
        out.push_str(&exp2.to_string());
    }
    out
}

fn quote_literal_value(val: TaggedValue) -> Result<Result<String, (i64, String)>, VmError> {
    if val.is_null() {
        return Ok(Ok("null".to_string()));
    }
    if let Some(b) = val.as_bool() {
        return Ok(Ok(if b { "true" } else { "false" }.to_string()));
    }
    // SAFETY: `val` was loaded from a rooted `BuiltinRoot` within this active call.
    if let Some(num) = unsafe { Number::from_tagged(val) } {
        let s = match num {
            Number::I64(v) => v.to_string(),
            Number::U64(v) => v.to_string(),
            Number::I128(v) => v.to_string(),
            Number::U128(v) => v.to_string(),
            Number::F64(x) => {
                if x.is_nan() {
                    "(0/0)".to_string()
                } else if x == f64::INFINITY {
                    "(1/0)".to_string()
                } else if x == f64::NEG_INFINITY {
                    "(-1/0)".to_string()
                } else {
                    // Exact IEEE-754 round-trip decimal representation (includes `.0` and `-0.0`).
                    format!("{x:?}")
                }
            }
        };
        return Ok(Ok(s));
    }
    if let Some(ch) = val.as_char() {
        return Ok(Ok(quote_string(&ch.to_string())));
    }
    if val.is_heap() {
        // Check if heap string via a helper without moving allocation.
        if let Some(text) = extract_heap_str_for_quote(val) {
            return Ok(Ok(quote_string(&text)));
        }
    }
    Ok(Err((
        FMT_ERR_TYPE_MISMATCH,
        "value has no safe literal representation for '%q'".to_string(),
    )))
}

fn extract_heap_str_for_quote(val: TaggedValue) -> Option<String> {
    let ptr = val.as_heap_ptr()?;
    // SAFETY: `val` is a rooted live VM value inside an active `BuiltinCtx` operation.
    let bytes = unsafe { gc::heap_string_bytes(ptr) }?;
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// 575..=579: Float formatting & frexp / ldexp
// ---------------------------------------------------------------------------

/// `575` `f64_to_fixed(x: f64, digits: i64) -> String`
pub fn f64_to_fixed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let x = ctx.arg_f64(0)?;
    let digits = ctx.arg_i64(1)?;
    if !(0..=100).contains(&digits) {
        return Err(VmError::InvalidArgument(
            "f64_to_fixed digits must be in 0..=100",
        ));
    }
    let s = if !x.is_finite() {
        format_non_finite(x)
    } else {
        let prec = digits as usize;
        let sign = if x.is_sign_negative() { "-" } else { "" };
        format!("{sign}{:.prec$}", x.abs())
    };
    ctx.return_string(&s)
}

/// `576` `f64_to_exp(x: f64, digits: i64) -> String`
pub fn f64_to_exp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let x = ctx.arg_f64(0)?;
    let digits = ctx.arg_i64(1)?;
    if digits != -1 && !(0..=100).contains(&digits) {
        return Err(VmError::InvalidArgument(
            "f64_to_exp digits must be -1 or in 0..=100",
        ));
    }
    let s = if !x.is_finite() {
        format_non_finite(x)
    } else {
        let sign = if x.is_sign_negative() { "-" } else { "" };
        let abs_x = x.abs();
        let raw = if digits == -1 {
            format!("{abs_x:e}")
        } else {
            let prec = digits as usize;
            format!("{abs_x:.prec$e}")
        };
        let (mant, exp_str) = raw.split_once('e').expect("exp format contains 'e'");
        let exp_val: i32 = exp_str.parse().unwrap_or(0);
        if exp_val < 0 {
            format!("{sign}{mant}e{exp_val}")
        } else {
            format!("{sign}{mant}e+{exp_val}")
        }
    };
    ctx.return_string(&s)
}

/// `577` `f64_to_precision(x: f64, prec: i64) -> String`
pub fn f64_to_precision(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let x = ctx.arg_f64(0)?;
    let prec = ctx.arg_i64(1)?;
    if !(1..=100).contains(&prec) {
        return Err(VmError::InvalidArgument(
            "f64_to_precision precision must be in 1..=100",
        ));
    }
    let s = if !x.is_finite() {
        format_non_finite(x)
    } else {
        format_precision_finite(x, prec as usize)
    };
    ctx.return_string(&s)
}

fn format_non_finite(x: f64) -> String {
    if x.is_nan() {
        "NaN".to_string()
    } else if x.is_sign_negative() {
        "-inf".to_string()
    } else {
        "inf".to_string()
    }
}

fn format_precision_finite(x: f64, p: usize) -> String {
    let sign = if x.is_sign_negative() { "-" } else { "" };
    let abs_x = x.abs();
    let raw = format!("{abs_x:.frac$e}", frac = p - 1);
    let (mant, exp_str) = raw.split_once('e').expect("exp format contains 'e'");
    let exp_val: i32 = exp_str.parse().unwrap_or(0);

    if exp_val < -6 || exp_val >= p as i32 {
        if exp_val < 0 {
            format!("{sign}{mant}e{exp_val}")
        } else {
            format!("{sign}{mant}e+{exp_val}")
        }
    } else {
        let mut digits = String::with_capacity(p);
        for ch in mant.chars() {
            if ch != '.' {
                digits.push(ch);
            }
        }
        let mut out = String::with_capacity(p + 8);
        out.push_str(sign);
        if exp_val < 0 {
            out.push_str("0.");
            for _ in 0..(-1 - exp_val) {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            let int_len = (exp_val + 1) as usize;
            out.push_str(&digits[..int_len]);
            if int_len < digits.len() {
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        }
        out
    }
}

/// `578` `f64_frexp(x: f64) -> (f64, i64)`
pub fn f64_frexp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let x = ctx.arg_f64(0)?;
    let (m, exp) = frexp_f64(x);
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::F64(m),
        NativeValue::I64(exp),
    ]))
}

fn frexp_f64(x: f64) -> (f64, i64) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let mut bits = x.to_bits();
    let sign = bits & (1u64 << 63);
    let mut biased = ((bits >> 52) & 0x7ff) as i64;
    let mut exp_adjust = 0i64;
    if biased == 0 {
        // Scale subnormal up by 2^54 (`0x4350_0000_0000_0000`) into normal range.
        let scaled = x * f64::from_bits(0x4350_0000_0000_0000);
        bits = scaled.to_bits();
        biased = ((bits >> 52) & 0x7ff) as i64;
        exp_adjust = -54;
    }
    let frac = bits & 0x000f_ffff_ffff_ffff;
    let exp = biased - 1022 + exp_adjust;
    let mantissa = f64::from_bits(sign | (1022u64 << 52) | frac);
    (mantissa, exp)
}

/// `579` `f64_ldexp(m: f64, exp: i64) -> f64`
pub fn f64_ldexp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let m = ctx.arg_f64(0)?;
    let exp = ctx.arg_i64(1)?;
    ctx.return_f64(ldexp_f64(m, exp))
}
