//! Built-in high-performance JSON kernel wrappers (`670..=687`).

use interpreter::{BuiltinCtx, VmError};

/// `670` `__json_parse_dynamic(text: String) -> (i64, i64, String, Any)`
pub fn json_parse_dynamic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_parse_dynamic()
}

/// `671` `__json_stringify_dynamic(value: Any, pretty: bool) -> (i64, String, String)`
pub fn json_stringify_dynamic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_stringify_dynamic()
}

/// `672` `__json_tape_open(text: String) -> (i64, i64, String)`
pub fn json_tape_open(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_open()
}

/// `673` `__json_tape_close(tape: i64) -> Unit`
pub fn json_tape_close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_close()
}

/// `674` `__json_tape_kind(tape: i64, node: i64) -> i64`
pub fn json_tape_kind(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_kind()
}

/// `675` `__json_tape_obj_get(tape: i64, node: i64, key: String) -> i64`
pub fn json_tape_obj_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_obj_get()
}

/// `676` `__json_tape_obj_unknown(tape: i64, node: i64, known_keys: Any) -> ?String`
pub fn json_tape_obj_unknown(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_obj_unknown()
}

/// `677` `__json_tape_obj_single_variant(tape: i64, node: i64) -> (i64, String)`
pub fn json_tape_obj_single_variant(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_obj_single_variant()
}

/// `678` `__json_tape_arr_len(tape: i64, node: i64) -> i64`
pub fn json_tape_arr_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_arr_len()
}

/// `679` `__json_tape_arr_at(tape: i64, node: i64, idx: i64) -> i64`
pub fn json_tape_arr_at(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_arr_at()
}

/// `680` `__json_tape_read_bool(tape: i64, node: i64) -> (i64, bool, String)`
pub fn json_tape_read_bool(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_read_bool()
}

/// `681` `__json_tape_read_int(tape: i64, node: i64, width_tag: i64) -> (i64, Any, String)`
pub fn json_tape_read_int(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_read_int()
}

/// `682` `__json_tape_read_float(tape: i64, node: i64, is_f32: bool) -> (i64, Any, String)`
pub fn json_tape_read_float(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_read_float()
}

/// `683` `__json_tape_read_str(tape: i64, node: i64, want_char: bool) -> (i64, Any, String)`
pub fn json_tape_read_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_tape_read_str()
}

/// `684` `__json_writer_new(indent: i64) -> i64`
pub fn json_writer_new(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_writer_new()
}

/// `685` `__json_writer_op(writer: i64, op: i64, arg: Any) -> i64`
pub fn json_writer_op(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_writer_op()
}

/// `686` `__json_writer_finish(writer: i64) -> (i64, String, String)`
pub fn json_writer_finish(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_writer_finish()
}

/// `687` `__json_writer_close(writer: i64) -> Unit`
pub fn json_writer_close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.json_writer_close()
}
