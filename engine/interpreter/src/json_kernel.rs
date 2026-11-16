//! Native RFC 8259 JSON tape scanner, streaming writer, and dynamic codec kernel.
//!
//! Host-side cursors (`JsonTape` and `JsonWriter`) live inside `VmState::json`
//! behind monotonic `i64` handles so raw Rust pointers never enter GC-traced
//! `TaggedValue` slots. Active heap-object ancestors tracked for `JsonWriter`
//! cycle detection live in `VmRoots::json_writer_ancestors` and are visited by
//! `scan_vm_roots` across moving Immix collections.

use std::collections::HashMap;
use std::io::Write as _;

use gc::ObjectHeader;
use runtime::{Number, TaggedValue};
use type_pool::{CollectionRole, Intrinsic};

use crate::builtin_ctx::{heap_string_to_owned, with_heap_string_str};
use crate::{BuiltinCtx, Vm, VmError};

/// Maximum nesting depth permitted by RFC 8259 parsing and serialization.
pub const MAX_JSON_DEPTH: usize = 128;

// ---------------------------------------------------------------------------
// Shared status / error codes (matched by `std.json` and `nir::json_derivation`)
// ---------------------------------------------------------------------------

pub const JSON_OK: i64 = 0;
pub const JSON_ERR_UNEXPECTED_EOF: i64 = 1;
pub const JSON_ERR_UNEXPECTED_TOKEN: i64 = 2;
pub const JSON_ERR_INVALID_NUMBER: i64 = 3;
pub const JSON_ERR_INVALID_STRING: i64 = 4;
pub const JSON_ERR_INVALID_ESCAPE: i64 = 5;
pub const JSON_ERR_TRAILING_CHARS: i64 = 6;
pub const JSON_ERR_UNSUPPORTED_VALUE: i64 = 7;
pub const JSON_ERR_DEPTH_LIMIT: i64 = 8;
pub const JSON_ERR_TYPE_MISMATCH: i64 = 9;
pub const JSON_ERR_MISSING_FIELD: i64 = 10;
pub const JSON_ERR_UNKNOWN_FIELD: i64 = 11;
pub const JSON_ERR_INVALID_VARIANT: i64 = 12;
pub const JSON_ERR_INT_OUT_OF_RANGE: i64 = 13;
pub const JSON_ERR_CYCLE_DETECTED: i64 = 14;

// ---------------------------------------------------------------------------
// Tape node kind tags (`__json_tape_kind`)
// ---------------------------------------------------------------------------

pub const TAPE_KIND_INVALID: i64 = 0;
pub const TAPE_KIND_NULL: i64 = 1;
pub const TAPE_KIND_BOOL: i64 = 2;
pub const TAPE_KIND_INT: i64 = 3;
pub const TAPE_KIND_FLOAT: i64 = 4;
pub const TAPE_KIND_STRING: i64 = 5;
pub const TAPE_KIND_ARRAY: i64 = 6;
pub const TAPE_KIND_OBJECT: i64 = 7;

// ---------------------------------------------------------------------------
// Writer opcodes (`__json_writer_op`)
// ---------------------------------------------------------------------------

pub const WRITER_OP_BEGIN_OBJ: i64 = 1;
pub const WRITER_OP_END_OBJ: i64 = 2;
pub const WRITER_OP_BEGIN_ARR: i64 = 3;
pub const WRITER_OP_END_ARR: i64 = 4;
pub const WRITER_OP_FIELD_KEY: i64 = 5;
pub const WRITER_OP_SCALAR: i64 = 6;
pub const WRITER_OP_RAW_JSON: i64 = 7;
pub const WRITER_OP_REFORMAT_JSON: i64 = 10;

// ---------------------------------------------------------------------------
// Integer width tags (`__json_tape_read_int`)
// ---------------------------------------------------------------------------

pub const INT_TAG_I8: i64 = 0;
pub const INT_TAG_I16: i64 = 1;
pub const INT_TAG_I32: i64 = 2;
pub const INT_TAG_I64: i64 = 3;
pub const INT_TAG_I128: i64 = 4;
pub const INT_TAG_ISIZE: i64 = 5;
pub const INT_TAG_U8: i64 = 6;
pub const INT_TAG_U16: i64 = 7;
pub const INT_TAG_U32: i64 = 8;
pub const INT_TAG_U64: i64 = 9;
pub const INT_TAG_U128: i64 = 10;
pub const INT_TAG_USIZE: i64 = 11;

const LARGE_OBJECT_HASH_THRESHOLD: usize = 16;
const HASH_EMPTY: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Host-side JSON state on `VmState`
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct VmJsonState {
    next_tape_id: i64,
    tapes: HashMap<i64, JsonTape>,
    next_writer_id: i64,
    writers: HashMap<i64, JsonWriter>,
}

impl VmJsonState {
    pub fn new() -> Self {
        Self {
            next_tape_id: 1,
            tapes: HashMap::new(),
            next_writer_id: 1,
            writers: HashMap::new(),
        }
    }

    pub fn active_tape_count(&self) -> usize {
        self.tapes.len()
    }

    pub fn active_writer_count(&self) -> usize {
        self.writers.len()
    }
}

// ---------------------------------------------------------------------------
// `JsonTape` representation and strict RFC 8259 scanner
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum StrRef {
    Input { offset: u32, len: u32 },
    Decoded { offset: u32, len: u32 },
}

#[derive(Clone, Copy)]
struct ObjEntry {
    key: StrRef,
    key_hash: u64,
    val_node: u32,
}

enum TapeNode {
    Null,
    Bool(bool),
    Number {
        offset: u32,
        len: u32,
        is_float: bool,
    },
    Str(StrRef),
    Array {
        start: u32,
        len: u32,
    },
    Object {
        start: u32,
        len: u32,
        hash_index: Option<Box<[u32]>>,
    },
}

struct JsonTape {
    input: Box<[u8]>,
    decoded: Vec<u8>,
    nodes: Vec<TapeNode>,
    array_children: Vec<u32>,
    object_entries: Vec<ObjEntry>,
}

struct ScanError {
    code: i64,
    offset: i64,
    message: String,
}

impl JsonTape {
    fn scan(bytes: &[u8]) -> Result<Self, ScanError> {
        if bytes.len() > u32::MAX as usize {
            return Err(ScanError {
                code: JSON_ERR_UNEXPECTED_TOKEN,
                offset: 0,
                message: "JSON document exceeds 4 GiB tape limit".to_string(),
            });
        }
        let mut tape = Self {
            input: Box::from(bytes),
            decoded: Vec::new(),
            nodes: Vec::with_capacity((bytes.len() / 16).max(8)),
            array_children: Vec::new(),
            object_entries: Vec::new(),
        };
        let mut pos = 0usize;
        // Reserve node 0 as the root slot so root index is always 0.
        tape.nodes.push(TapeNode::Null);
        let root_node = tape.scan_value(&mut pos, 0)?;
        tape.nodes.swap(0, root_node as usize);
        // Fix up any internal child references if root had children: note that only
        // root_node (pushed last) swapped with 0, and no parent points to root_node
        // or 0 because 0 was an unreferenced placeholder and root_node is the root!
        skip_ws(&tape.input, &mut pos);
        if pos < tape.input.len() {
            return Err(ScanError {
                code: JSON_ERR_TRAILING_CHARS,
                offset: pos as i64,
                message: String::new(),
            });
        }
        Ok(tape)
    }

    fn str_bytes(&self, r: StrRef) -> &[u8] {
        match r {
            StrRef::Input { offset, len } => {
                &self.input[offset as usize..(offset as usize + len as usize)]
            }
            StrRef::Decoded { offset, len } => {
                &self.decoded[offset as usize..(offset as usize + len as usize)]
            }
        }
    }

    fn str_slice(&self, r: StrRef) -> &str {
        // Guaranteed valid UTF-8: `Input` comes from a verified Nessa `String`
        // sliced on ASCII `"` / `\` boundaries, and `Decoded` only pushes valid UTF-8.
        std::str::from_utf8(self.str_bytes(r)).unwrap_or("")
    }

    fn node_kind_tag(&self, idx: usize) -> i64 {
        match self.nodes.get(idx) {
            None => TAPE_KIND_INVALID,
            Some(TapeNode::Null) => TAPE_KIND_NULL,
            Some(TapeNode::Bool(_)) => TAPE_KIND_BOOL,
            Some(TapeNode::Number {
                is_float: false, ..
            }) => TAPE_KIND_INT,
            Some(TapeNode::Number { is_float: true, .. }) => TAPE_KIND_FLOAT,
            Some(TapeNode::Str(_)) => TAPE_KIND_STRING,
            Some(TapeNode::Array { .. }) => TAPE_KIND_ARRAY,
            Some(TapeNode::Object { .. }) => TAPE_KIND_OBJECT,
        }
    }

    fn node_kind_name(&self, idx: usize) -> &'static str {
        match self.nodes.get(idx) {
            None => "invalid",
            Some(TapeNode::Null) => "null",
            Some(TapeNode::Bool(_)) => "bool",
            Some(TapeNode::Number {
                is_float: false, ..
            }) => "integer",
            Some(TapeNode::Number { is_float: true, .. }) => "float",
            Some(TapeNode::Str(_)) => "string",
            Some(TapeNode::Array { .. }) => "array",
            Some(TapeNode::Object { .. }) => "object",
        }
    }

    fn object_lookup(&mut self, node_idx: usize, key: &[u8]) -> Option<u32> {
        let (start, len, need_build) = match self.nodes.get(node_idx)? {
            TapeNode::Object {
                start,
                len,
                hash_index,
            } => (
                *start as usize,
                *len as usize,
                *len as usize > LARGE_OBJECT_HASH_THRESHOLD && hash_index.is_none(),
            ),
            _ => return None,
        };
        if len == 0 {
            return None;
        }
        if len <= LARGE_OBJECT_HASH_THRESHOLD {
            for i in 0..len {
                let entry = self.object_entries[start + i];
                if self.str_bytes(entry.key) == key {
                    return Some(entry.val_node);
                }
            }
            return None;
        }
        if need_build {
            let cap = (len * 2).next_power_of_two();
            let mask = cap - 1;
            let mut table = vec![HASH_EMPTY; cap].into_boxed_slice();
            for rel in 0..len {
                let entry = self.object_entries[start + rel];
                let mut slot = (entry.key_hash as usize) & mask;
                while table[slot] != HASH_EMPTY {
                    slot = (slot + 1) & mask;
                }
                table[slot] = rel as u32;
            }
            if let Some(TapeNode::Object { hash_index, .. }) = self.nodes.get_mut(node_idx) {
                *hash_index = Some(table);
            }
        }
        let hash = fnv1a_bytes(key);
        let Some(TapeNode::Object {
            hash_index: Some(table),
            ..
        }) = self.nodes.get(node_idx)
        else {
            return None;
        };
        let mask = table.len() - 1;
        let mut slot = (hash as usize) & mask;
        loop {
            let rel = table[slot];
            if rel == HASH_EMPTY {
                return None;
            }
            let entry = self.object_entries[start + rel as usize];
            if entry.key_hash == hash && self.str_bytes(entry.key) == key {
                return Some(entry.val_node);
            }
            slot = (slot + 1) & mask;
        }
    }

    fn push_node(&mut self, node: TapeNode) -> u32 {
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        idx
    }

    fn scan_value(&mut self, pos: &mut usize, depth: usize) -> Result<u32, ScanError> {
        if depth > MAX_JSON_DEPTH {
            return Err(ScanError {
                code: JSON_ERR_DEPTH_LIMIT,
                offset: *pos as i64,
                message: String::new(),
            });
        }
        skip_ws(&self.input, pos);
        if *pos >= self.input.len() {
            return Err(ScanError {
                code: JSON_ERR_UNEXPECTED_EOF,
                offset: *pos as i64,
                message: String::new(),
            });
        }
        match self.input[*pos] {
            b'n' => {
                if self.input[*pos..].starts_with(b"null") {
                    *pos += 4;
                    Ok(self.push_node(TapeNode::Null))
                } else {
                    Err(ScanError {
                        code: JSON_ERR_UNEXPECTED_TOKEN,
                        offset: *pos as i64,
                        message: "invalid literal, expected 'null'".to_string(),
                    })
                }
            }
            b't' => {
                if self.input[*pos..].starts_with(b"true") {
                    *pos += 4;
                    Ok(self.push_node(TapeNode::Bool(true)))
                } else {
                    Err(ScanError {
                        code: JSON_ERR_UNEXPECTED_TOKEN,
                        offset: *pos as i64,
                        message: "invalid literal, expected 'true'".to_string(),
                    })
                }
            }
            b'f' => {
                if self.input[*pos..].starts_with(b"false") {
                    *pos += 5;
                    Ok(self.push_node(TapeNode::Bool(false)))
                } else {
                    Err(ScanError {
                        code: JSON_ERR_UNEXPECTED_TOKEN,
                        offset: *pos as i64,
                        message: "invalid literal, expected 'false'".to_string(),
                    })
                }
            }
            b'"' => {
                let sref = self.scan_string(pos)?;
                Ok(self.push_node(TapeNode::Str(sref)))
            }
            b'[' => self.scan_array(pos, depth),
            b'{' => self.scan_object(pos, depth),
            b'-' | b'0'..=b'9' => self.scan_number(pos),
            b'+' => Err(ScanError {
                code: JSON_ERR_INVALID_NUMBER,
                offset: *pos as i64,
                message: "leading '+' is not permitted in JSON numbers".to_string(),
            }),
            other => Err(ScanError {
                code: JSON_ERR_UNEXPECTED_TOKEN,
                offset: *pos as i64,
                message: format!("unexpected byte 0x{other:02x}"),
            }),
        }
    }

    fn scan_string(&mut self, pos: &mut usize) -> Result<StrRef, ScanError> {
        debug_assert_eq!(self.input[*pos], b'"');
        *pos += 1;
        let content_start = *pos;

        // Fast path: scan with memchr2 for `"` or `\` while checking control chars `< 0x20`.
        if content_start >= self.input.len() {
            return Err(ScanError {
                code: JSON_ERR_UNEXPECTED_EOF,
                offset: self.input.len() as i64,
                message: String::new(),
            });
        }
        let slice = &self.input[content_start..];
        let mut cursor = match memchr::memchr2(b'"', b'\\', slice) {
            Some(rel) => {
                if let Some(ctrl_rel) = find_control_byte(&slice[..rel]) {
                    return Err(ScanError {
                        code: JSON_ERR_INVALID_STRING,
                        offset: (content_start + ctrl_rel) as i64,
                        message: "unescaped control character in JSON string".to_string(),
                    });
                }
                let hit = content_start + rel;
                if self.input[hit] == b'"' {
                    *pos = hit + 1;
                    return Ok(StrRef::Input {
                        offset: content_start as u32,
                        len: (hit - content_start) as u32,
                    });
                }
                // Hit backslash `\` at `hit` — switch to escaped decoding into `self.decoded`.
                hit
            }
            None => {
                if let Some(ctrl_rel) = find_control_byte(slice) {
                    return Err(ScanError {
                        code: JSON_ERR_INVALID_STRING,
                        offset: (content_start + ctrl_rel) as i64,
                        message: "unescaped control character in JSON string".to_string(),
                    });
                }
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: self.input.len() as i64,
                    message: String::new(),
                });
            }
        };

        let dec_start = self.decoded.len();
        if cursor > content_start {
            self.decoded
                .extend_from_slice(&self.input[content_start..cursor]);
        }

        loop {
            if cursor >= self.input.len() {
                self.decoded.truncate(dec_start);
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: self.input.len() as i64,
                    message: String::new(),
                });
            }
            let b = self.input[cursor];
            if b == b'"' {
                *pos = cursor + 1;
                let dec_len = self.decoded.len() - dec_start;
                return Ok(StrRef::Decoded {
                    offset: dec_start as u32,
                    len: dec_len as u32,
                });
            }
            if b < 0x20 {
                self.decoded.truncate(dec_start);
                return Err(ScanError {
                    code: JSON_ERR_INVALID_STRING,
                    offset: cursor as i64,
                    message: "unescaped control character in JSON string".to_string(),
                });
            }
            if b != b'\\' {
                // Copy contiguous unescaped run via memchr2.
                let slice = &self.input[cursor..];
                let end_rel = memchr::memchr2(b'"', b'\\', slice).unwrap_or(slice.len());
                if let Some(ctrl_rel) = find_control_byte(&slice[..end_rel]) {
                    self.decoded.truncate(dec_start);
                    return Err(ScanError {
                        code: JSON_ERR_INVALID_STRING,
                        offset: (cursor + ctrl_rel) as i64,
                        message: "unescaped control character in JSON string".to_string(),
                    });
                }
                self.decoded.extend_from_slice(&slice[..end_rel]);
                cursor += end_rel;
                continue;
            }

            let esc_offset = cursor;
            cursor += 1;
            if cursor >= self.input.len() {
                self.decoded.truncate(dec_start);
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: cursor as i64,
                    message: String::new(),
                });
            }
            let eb = self.input[cursor];
            cursor += 1;
            match eb {
                b'"' => self.decoded.push(b'"'),
                b'\\' => self.decoded.push(b'\\'),
                b'/' => self.decoded.push(b'/'),
                b'b' => self.decoded.push(0x08),
                b'f' => self.decoded.push(0x0c),
                b'n' => self.decoded.push(b'\n'),
                b'r' => self.decoded.push(b'\r'),
                b't' => self.decoded.push(b'\t'),
                b'u' => {
                    let u1 = match read_hex4(&self.input, &mut cursor, esc_offset) {
                        Ok(v) => v,
                        Err(e) => {
                            self.decoded.truncate(dec_start);
                            return Err(e);
                        }
                    };
                    if (0xDC00..=0xDFFF).contains(&u1) {
                        self.decoded.truncate(dec_start);
                        return Err(ScanError {
                            code: JSON_ERR_INVALID_ESCAPE,
                            offset: esc_offset as i64,
                            message: "unpaired UTF-16 low surrogate".to_string(),
                        });
                    }
                    let codepoint = if (0xD800..=0xDBFF).contains(&u1) {
                        if cursor + 2 > self.input.len()
                            || self.input[cursor] != b'\\'
                            || self.input[cursor + 1] != b'u'
                        {
                            self.decoded.truncate(dec_start);
                            return Err(ScanError {
                                code: JSON_ERR_INVALID_ESCAPE,
                                offset: esc_offset as i64,
                                message: "unpaired UTF-16 high surrogate".to_string(),
                            });
                        }
                        let low_esc_offset = cursor;
                        cursor += 2;
                        let u2 = match read_hex4(&self.input, &mut cursor, low_esc_offset) {
                            Ok(v) => v,
                            Err(e) => {
                                self.decoded.truncate(dec_start);
                                return Err(e);
                            }
                        };
                        if !(0xDC00..=0xDFFF).contains(&u2) {
                            self.decoded.truncate(dec_start);
                            return Err(ScanError {
                                code: JSON_ERR_INVALID_ESCAPE,
                                offset: low_esc_offset as i64,
                                message: "expected UTF-16 low surrogate".to_string(),
                            });
                        }
                        0x10000 + (((u1 as u32 - 0xD800) << 10) | (u2 as u32 - 0xDC00))
                    } else {
                        u1 as u32
                    };
                    let Some(ch) = char::from_u32(codepoint) else {
                        self.decoded.truncate(dec_start);
                        return Err(ScanError {
                            code: JSON_ERR_INVALID_ESCAPE,
                            offset: esc_offset as i64,
                            message: "invalid Unicode scalar value".to_string(),
                        });
                    };
                    let mut utf8 = [0u8; 4];
                    self.decoded
                        .extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                }
                _ => {
                    self.decoded.truncate(dec_start);
                    return Err(ScanError {
                        code: JSON_ERR_INVALID_ESCAPE,
                        offset: esc_offset as i64,
                        message: "invalid escape sequence in JSON string".to_string(),
                    });
                }
            }
        }
    }

    fn scan_number(&mut self, pos: &mut usize) -> Result<u32, ScanError> {
        let start = *pos;
        let mut is_float = false;

        if self.input[*pos] == b'-' {
            *pos += 1;
            if *pos >= self.input.len() {
                return Err(ScanError {
                    code: JSON_ERR_INVALID_NUMBER,
                    offset: start as i64,
                    message: "expected digit after '-'".to_string(),
                });
            }
        }

        let first = self.input[*pos];
        if !first.is_ascii_digit() {
            return Err(ScanError {
                code: JSON_ERR_INVALID_NUMBER,
                offset: start as i64,
                message: "expected digit in number".to_string(),
            });
        }
        if first == b'0' {
            *pos += 1;
            if *pos < self.input.len() && self.input[*pos].is_ascii_digit() {
                return Err(ScanError {
                    code: JSON_ERR_INVALID_NUMBER,
                    offset: start as i64,
                    message: "leading zeros are not permitted in JSON numbers".to_string(),
                });
            }
        } else {
            while *pos < self.input.len() && self.input[*pos].is_ascii_digit() {
                *pos += 1;
            }
        }

        if *pos < self.input.len() && self.input[*pos] == b'.' {
            is_float = true;
            *pos += 1;
            if *pos >= self.input.len() || !self.input[*pos].is_ascii_digit() {
                return Err(ScanError {
                    code: JSON_ERR_INVALID_NUMBER,
                    offset: start as i64,
                    message: "expected digit after decimal point".to_string(),
                });
            }
            while *pos < self.input.len() && self.input[*pos].is_ascii_digit() {
                *pos += 1;
            }
        }

        if *pos < self.input.len() && matches!(self.input[*pos], b'e' | b'E') {
            is_float = true;
            *pos += 1;
            if *pos < self.input.len() && matches!(self.input[*pos], b'+' | b'-') {
                *pos += 1;
            }
            if *pos >= self.input.len() || !self.input[*pos].is_ascii_digit() {
                return Err(ScanError {
                    code: JSON_ERR_INVALID_NUMBER,
                    offset: start as i64,
                    message: "expected digit in exponent".to_string(),
                });
            }
            while *pos < self.input.len() && self.input[*pos].is_ascii_digit() {
                *pos += 1;
            }
        }

        let len = *pos - start;
        // For float tokens, eagerly verify IEEE-754 finiteness (rejects `1e9999` at parse time
        // per RFC 8259 / existing std.json contract). Note that large exact integers (e.g.
        // 39-digit i128/u128) are NOT rejected here if <= f64::MAX (~1.79e308, 309 digits);
        // >309-digit numbers also fail finiteness cleanly.
        let token_str = std::str::from_utf8(&self.input[start..*pos]).unwrap_or("");
        match token_str.parse::<f64>() {
            Ok(v) if v.is_finite() => {}
            _ => {
                return Err(ScanError {
                    code: JSON_ERR_INVALID_NUMBER,
                    offset: start as i64,
                    message: "number out of IEEE-754 f64 range".to_string(),
                });
            }
        }

        Ok(self.push_node(TapeNode::Number {
            offset: start as u32,
            len: len as u32,
            is_float,
        }))
    }

    fn scan_array(&mut self, pos: &mut usize, depth: usize) -> Result<u32, ScanError> {
        if depth + 1 > MAX_JSON_DEPTH {
            return Err(ScanError {
                code: JSON_ERR_DEPTH_LIMIT,
                offset: *pos as i64,
                message: String::new(),
            });
        }
        *pos += 1;
        skip_ws(&self.input, pos);
        if *pos >= self.input.len() {
            return Err(ScanError {
                code: JSON_ERR_UNEXPECTED_EOF,
                offset: *pos as i64,
                message: String::new(),
            });
        }
        if self.input[*pos] == b']' {
            *pos += 1;
            return Ok(self.push_node(TapeNode::Array { start: 0, len: 0 }));
        }

        let mut local_children = Vec::new();
        loop {
            let child = self.scan_value(pos, depth + 1)?;
            local_children.push(child);
            skip_ws(&self.input, pos);
            if *pos >= self.input.len() {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: *pos as i64,
                    message: String::new(),
                });
            }
            let sep = self.input[*pos];
            if sep == b']' {
                *pos += 1;
                break;
            }
            if sep != b',' {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_TOKEN,
                    offset: *pos as i64,
                    message: "expected ',' or ']' in array".to_string(),
                });
            }
            *pos += 1;
            skip_ws(&self.input, pos);
            if *pos >= self.input.len() {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: *pos as i64,
                    message: String::new(),
                });
            }
            if self.input[*pos] == b']' {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_TOKEN,
                    offset: *pos as i64,
                    message: "trailing comma is not allowed in JSON array".to_string(),
                });
            }
        }

        let start = self.array_children.len() as u32;
        let len = local_children.len() as u32;
        self.array_children.extend_from_slice(&local_children);
        Ok(self.push_node(TapeNode::Array { start, len }))
    }

    fn scan_object(&mut self, pos: &mut usize, depth: usize) -> Result<u32, ScanError> {
        if depth + 1 > MAX_JSON_DEPTH {
            return Err(ScanError {
                code: JSON_ERR_DEPTH_LIMIT,
                offset: *pos as i64,
                message: String::new(),
            });
        }
        *pos += 1;
        skip_ws(&self.input, pos);
        if *pos >= self.input.len() {
            return Err(ScanError {
                code: JSON_ERR_UNEXPECTED_EOF,
                offset: *pos as i64,
                message: String::new(),
            });
        }
        if self.input[*pos] == b'}' {
            *pos += 1;
            return Ok(self.push_node(TapeNode::Object {
                start: 0,
                len: 0,
                hash_index: None,
            }));
        }

        let mut local_entries: Vec<ObjEntry> = Vec::new();
        loop {
            if self.input[*pos] != b'"' {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_TOKEN,
                    offset: *pos as i64,
                    message: "expected quoted property key in object".to_string(),
                });
            }
            let key_ref = self.scan_string(pos)?;
            let key_hash = fnv1a_bytes(self.str_bytes(key_ref));
            skip_ws(&self.input, pos);
            if *pos >= self.input.len() {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: *pos as i64,
                    message: String::new(),
                });
            }
            if self.input[*pos] != b':' {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_TOKEN,
                    offset: *pos as i64,
                    message: "expected ':' after object key".to_string(),
                });
            }
            *pos += 1;
            let val_node = self.scan_value(pos, depth + 1)?;

            // Last duplicate key wins (matching RFC 8259 / Map.set).
            let mut replaced = false;
            for existing in &mut local_entries {
                if existing.key_hash == key_hash
                    && self.str_bytes(existing.key) == self.str_bytes(key_ref)
                {
                    existing.val_node = val_node;
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                local_entries.push(ObjEntry {
                    key: key_ref,
                    key_hash,
                    val_node,
                });
            }

            skip_ws(&self.input, pos);
            if *pos >= self.input.len() {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: *pos as i64,
                    message: String::new(),
                });
            }
            let sep = self.input[*pos];
            if sep == b'}' {
                *pos += 1;
                break;
            }
            if sep != b',' {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_TOKEN,
                    offset: *pos as i64,
                    message: "expected ',' or '}' in object".to_string(),
                });
            }
            *pos += 1;
            skip_ws(&self.input, pos);
            if *pos >= self.input.len() {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_EOF,
                    offset: *pos as i64,
                    message: String::new(),
                });
            }
            if self.input[*pos] == b'}' {
                return Err(ScanError {
                    code: JSON_ERR_UNEXPECTED_TOKEN,
                    offset: *pos as i64,
                    message: "trailing comma is not allowed in JSON object".to_string(),
                });
            }
        }

        let start = self.object_entries.len() as u32;
        let len = local_entries.len() as u32;
        self.object_entries.extend_from_slice(&local_entries);
        Ok(self.push_node(TapeNode::Object {
            start,
            len,
            hash_index: None,
        }))
    }
}

#[inline]
fn skip_ws(bytes: &[u8], pos: &mut usize) {
    // SWAR-friendly tight byte loop over ASCII whitespace (0x20, 0x09, 0x0A, 0x0D).
    let mut i = *pos;
    let n = bytes.len();
    while i < n {
        let b = bytes[i];
        if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
            i += 1;
        } else {
            break;
        }
    }
    *pos = i;
}

#[inline]
fn find_control_byte(bytes: &[u8]) -> Option<usize> {
    // Process 8 bytes at a time via SWAR check for any byte < 0x20.
    let mut i = 0;
    while i + 8 <= bytes.len() {
        let chunk = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        // Subtracting 0x20 from each byte sets the high bit if byte < 0x20 and ASCII (< 0x80).
        let has_lt_20 = chunk.wrapping_sub(0x2020_2020_2020_2020) & !chunk & 0x8080_8080_8080_8080;
        if has_lt_20 != 0 {
            for k in 0..8 {
                if bytes[i + k] < 0x20 {
                    return Some(i + k);
                }
            }
        }
        i += 8;
    }
    while i < bytes.len() {
        if bytes[i] < 0x20 {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[inline]
fn read_hex4(bytes: &[u8], pos: &mut usize, esc_offset: usize) -> Result<u16, ScanError> {
    if *pos + 4 > bytes.len() {
        return Err(ScanError {
            code: JSON_ERR_INVALID_ESCAPE,
            offset: esc_offset as i64,
            message: "incomplete \\uXXXX escape".to_string(),
        });
    }
    let mut val = 0u16;
    for k in 0..4 {
        let nib = match bytes[*pos + k] {
            b'0'..=b'9' => bytes[*pos + k] - b'0',
            b'A'..=b'F' => bytes[*pos + k] - b'A' + 10,
            b'a'..=b'f' => bytes[*pos + k] - b'a' + 10,
            _ => {
                return Err(ScanError {
                    code: JSON_ERR_INVALID_ESCAPE,
                    offset: esc_offset as i64,
                    message: "invalid hex digit in \\uXXXX escape".to_string(),
                });
            }
        };
        val = (val << 4) | u16::from(nib);
    }
    *pos += 4;
    Ok(val)
}

#[inline]
fn fnv1a_bytes(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------
// Exact 12-width integer parser
// ---------------------------------------------------------------------------

enum ParsedInt {
    Signed(i128),
    Unsigned(u128),
}

fn width_tag_intrinsic(width_tag: i64) -> Option<Intrinsic> {
    match width_tag {
        INT_TAG_I8 => Some(Intrinsic::I8),
        INT_TAG_I16 => Some(Intrinsic::I16),
        INT_TAG_I32 => Some(Intrinsic::I32),
        INT_TAG_I64 => Some(Intrinsic::I64),
        INT_TAG_I128 => Some(Intrinsic::I128),
        INT_TAG_ISIZE => Some(Intrinsic::Isize),
        INT_TAG_U8 => Some(Intrinsic::U8),
        INT_TAG_U16 => Some(Intrinsic::U16),
        INT_TAG_U32 => Some(Intrinsic::U32),
        INT_TAG_U64 => Some(Intrinsic::U64),
        INT_TAG_U128 => Some(Intrinsic::U128),
        INT_TAG_USIZE => Some(Intrinsic::Usize),
        _ => None,
    }
}

fn parse_exact_int(bytes: &[u8], kind: Intrinsic) -> Result<ParsedInt, ()> {
    if bytes.is_empty() {
        return Err(());
    }
    let negative = bytes[0] == b'-';
    let digits = if negative { &bytes[1..] } else { bytes };
    if digits.is_empty() {
        return Err(());
    }
    let mut mag: u128 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return Err(());
        }
        mag = mag.checked_mul(10).ok_or(())?;
        mag = mag.checked_add(u128::from(b - b'0')).ok_or(())?;
    }

    let is_unsigned = matches!(
        kind,
        Intrinsic::U8
            | Intrinsic::U16
            | Intrinsic::U32
            | Intrinsic::U64
            | Intrinsic::U128
            | Intrinsic::Usize
    );

    if negative && mag == 0 {
        // `-0` represents integer zero in RFC 8259.
        return if is_unsigned {
            Ok(ParsedInt::Unsigned(0))
        } else {
            Ok(ParsedInt::Signed(0))
        };
    }

    if is_unsigned {
        if negative {
            return Err(());
        }
        let max: u128 = match kind {
            Intrinsic::U8 => u8::MAX as u128,
            Intrinsic::U16 => u16::MAX as u128,
            Intrinsic::U32 => u32::MAX as u128,
            Intrinsic::U64 => u64::MAX as u128,
            Intrinsic::Usize => usize::MAX as u128,
            Intrinsic::U128 => u128::MAX,
            _ => unreachable!(),
        };
        if mag > max {
            return Err(());
        }
        Ok(ParsedInt::Unsigned(mag))
    } else if negative {
        let max_neg_mag: u128 = match kind {
            Intrinsic::I8 => (i8::MAX as u128) + 1,
            Intrinsic::I16 => (i16::MAX as u128) + 1,
            Intrinsic::I32 => (i32::MAX as u128) + 1,
            Intrinsic::I64 => (i64::MAX as u128) + 1,
            Intrinsic::Isize => (isize::MAX as u128) + 1,
            Intrinsic::I128 => (i128::MAX as u128) + 1,
            _ => unreachable!(),
        };
        if mag > max_neg_mag {
            return Err(());
        }
        if mag == (i128::MAX as u128) + 1 {
            Ok(ParsedInt::Signed(i128::MIN))
        } else {
            Ok(ParsedInt::Signed(-(mag as i128)))
        }
    } else {
        let max_pos: u128 = match kind {
            Intrinsic::I8 => i8::MAX as u128,
            Intrinsic::I16 => i16::MAX as u128,
            Intrinsic::I32 => i32::MAX as u128,
            Intrinsic::I64 => i64::MAX as u128,
            Intrinsic::Isize => isize::MAX as u128,
            Intrinsic::I128 => i128::MAX as u128,
            _ => unreachable!(),
        };
        if mag > max_pos {
            return Err(());
        }
        Ok(ParsedInt::Signed(mag as i128))
    }
}

fn alloc_parsed_int(
    vm: &mut Vm,
    kind: Intrinsic,
    parsed: ParsedInt,
) -> Result<TaggedValue, VmError> {
    let number = match (kind, parsed) {
        (Intrinsic::I128, ParsedInt::Signed(v)) => Number::I128(v),
        (Intrinsic::U128, ParsedInt::Unsigned(v)) => Number::U128(v),
        (
            Intrinsic::I8 | Intrinsic::I16 | Intrinsic::I32 | Intrinsic::I64 | Intrinsic::Isize,
            ParsedInt::Signed(v),
        ) => Number::I64(v as i64),
        (
            Intrinsic::U8 | Intrinsic::U16 | Intrinsic::U32 | Intrinsic::U64 | Intrinsic::Usize,
            ParsedInt::Unsigned(v),
        ) => Number::U64(v as u64),
        _ => return Err(VmError::TypeError),
    };
    vm.typed_number_value(number, kind)
}

// ---------------------------------------------------------------------------
// `JsonWriter` streaming serializer
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum ContainerState {
    Obj { count: usize, expecting_val: bool },
    Arr { count: usize },
}

struct JsonWriter {
    buf: Vec<u8>,
    indent: usize,
    pretty: bool,
    stack: Vec<ContainerState>,
    err: Option<(i64, String)>,
}

impl JsonWriter {
    fn new(indent: i64) -> Self {
        let pretty = indent > 0;
        let indent_spaces = if pretty { (indent.min(64)) as usize } else { 0 };
        Self {
            buf: Vec::with_capacity(128),
            indent: indent_spaces,
            pretty,
            stack: Vec::with_capacity(16),
            err: None,
        }
    }

    fn set_err(&mut self, code: i64, msg: impl Into<String>) -> i64 {
        if self.err.is_none() {
            self.err = Some((code, msg.into()));
        }
        self.err.as_ref().unwrap().0
    }

    fn status(&self) -> i64 {
        self.err.as_ref().map_or(JSON_OK, |(c, _)| *c)
    }

    fn write_indent(&mut self, depth: usize) {
        let total = depth.saturating_mul(self.indent);
        const SPACES: &[u8; 64] = &[b' '; 64];
        let mut rem = total;
        while rem >= 64 {
            self.buf.extend_from_slice(SPACES);
            rem -= 64;
        }
        if rem > 0 {
            self.buf.extend_from_slice(&SPACES[..rem]);
        }
    }

    fn before_value(&mut self) {
        let depth = self.stack.len();
        match self.stack.last_mut() {
            Some(ContainerState::Obj { expecting_val, .. }) => {
                *expecting_val = false;
            }
            Some(ContainerState::Arr { count }) => {
                let c = *count;
                *count = c + 1;
                if c > 0 {
                    self.buf.push(b',');
                }
                if self.pretty {
                    self.buf.push(b'\n');
                    self.write_indent(depth);
                }
            }
            None => {}
        }
    }

    fn begin_obj(&mut self) -> i64 {
        if self.err.is_some() {
            return self.status();
        }
        if self.stack.len() + 1 > MAX_JSON_DEPTH {
            return self.set_err(JSON_ERR_DEPTH_LIMIT, "");
        }
        self.before_value();
        self.buf.push(b'{');
        self.stack.push(ContainerState::Obj {
            count: 0,
            expecting_val: false,
        });
        JSON_OK
    }

    fn end_obj(&mut self) -> i64 {
        if self.err.is_some() {
            return self.status();
        }
        let Some(ContainerState::Obj { count, .. }) = self.stack.pop() else {
            return self.set_err(JSON_ERR_UNSUPPORTED_VALUE, "unbalanced JSON object end");
        };
        if count > 0 && self.pretty {
            self.buf.push(b'\n');
            self.write_indent(self.stack.len());
        }
        self.buf.push(b'}');
        JSON_OK
    }

    fn begin_arr(&mut self) -> i64 {
        if self.err.is_some() {
            return self.status();
        }
        if self.stack.len() + 1 > MAX_JSON_DEPTH {
            return self.set_err(JSON_ERR_DEPTH_LIMIT, "");
        }
        self.before_value();
        self.buf.push(b'[');
        self.stack.push(ContainerState::Arr { count: 0 });
        JSON_OK
    }

    fn end_arr(&mut self) -> i64 {
        if self.err.is_some() {
            return self.status();
        }
        let Some(ContainerState::Arr { count }) = self.stack.pop() else {
            return self.set_err(JSON_ERR_UNSUPPORTED_VALUE, "unbalanced JSON array end");
        };
        if count > 0 && self.pretty {
            self.buf.push(b'\n');
            self.write_indent(self.stack.len());
        }
        self.buf.push(b']');
        JSON_OK
    }

    fn write_field_key(&mut self, key: &str) -> i64 {
        if self.err.is_some() {
            return self.status();
        }
        let depth = self.stack.len();
        let Some(ContainerState::Obj {
            count,
            expecting_val,
        }) = self.stack.last_mut()
        else {
            return self.set_err(JSON_ERR_UNSUPPORTED_VALUE, "field key outside JSON object");
        };
        let c = *count;
        *count = c + 1;
        *expecting_val = true;
        if c > 0 {
            self.buf.push(b',');
        }
        if self.pretty {
            self.buf.push(b'\n');
            self.write_indent(depth);
        }
        write_escaped_json_str(&mut self.buf, key.as_bytes());
        if self.pretty {
            self.buf.extend_from_slice(b": ");
        } else {
            self.buf.push(b':');
        }
        JSON_OK
    }

    fn write_raw_json(&mut self, raw: &[u8]) -> i64 {
        if self.err.is_some() {
            return self.status();
        }
        let base_depth = self.stack.len();
        self.before_value();
        if let Err((code, msg)) = reformat_json_into(
            raw,
            &mut self.buf,
            if self.pretty { self.indent } else { 0 },
            base_depth,
        ) {
            return self.set_err(code, msg);
        }
        JSON_OK
    }
}

pub(crate) fn write_escaped_json_str(buf: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    buf.push(b'"');
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\\' || b < 0x20 {
            if i > start {
                buf.extend_from_slice(&bytes[start..i]);
            }
            match b {
                b'"' => buf.extend_from_slice(b"\\\""),
                b'\\' => buf.extend_from_slice(b"\\\\"),
                0x08 => buf.extend_from_slice(b"\\b"),
                0x0c => buf.extend_from_slice(b"\\f"),
                b'\n' => buf.extend_from_slice(b"\\n"),
                b'\r' => buf.extend_from_slice(b"\\r"),
                b'\t' => buf.extend_from_slice(b"\\t"),
                ctrl => {
                    buf.extend_from_slice(&[
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[(ctrl >> 4) as usize],
                        HEX[(ctrl & 0x0f) as usize],
                    ]);
                }
            }
            i += 1;
            start = i;
        } else {
            // Fast skip until next `"`, `\`, or control byte.
            let slice = &bytes[i..];
            let stop = memchr::memchr2(b'"', b'\\', slice).unwrap_or(slice.len());
            if let Some(ctrl_rel) = find_control_byte(&slice[..stop]) {
                i += ctrl_rel;
            } else {
                i += stop;
            }
        }
    }
    if bytes.len() > start {
        buf.extend_from_slice(&bytes[start..]);
    }
    buf.push(b'"');
}

/// Streaming validator/pretty-reformatter for raw JSON fragments (`to_json_pretty` & custom `impl Json`).
/// Preserves exact number/string tokens (including 128-bit integers) without lossy `f64` conversion.
fn reformat_json_into(
    raw: &[u8],
    out: &mut Vec<u8>,
    indent: usize,
    base_depth: usize,
) -> Result<(), (i64, String)> {
    // Validate first via `JsonTape::scan` when `base_depth == 0` or check depth/tokens cleanly.
    let pretty = indent > 0;
    let mut pos = 0usize;
    let mut stack: Vec<(u8, usize)> = Vec::new(); // (b'{' or b'[', item_count)

    let write_ind = |buf: &mut Vec<u8>, level: usize| {
        let total = level.saturating_mul(indent);
        const SPACES: &[u8; 64] = &[b' '; 64];
        let mut rem = total;
        while rem >= 64 {
            buf.extend_from_slice(SPACES);
            rem -= 64;
        }
        if rem > 0 {
            buf.extend_from_slice(&SPACES[..rem]);
        }
    };

    skip_ws(raw, &mut pos);
    if pos >= raw.len() {
        return Err((
            JSON_ERR_UNSUPPORTED_VALUE,
            "custom to_json() returned empty JSON".to_string(),
        ));
    }

    while pos < raw.len() {
        skip_ws(raw, &mut pos);
        if pos >= raw.len() {
            break;
        }
        let b = raw[pos];
        match b {
            b'{' | b'[' => {
                if base_depth + stack.len() + 1 > MAX_JSON_DEPTH {
                    return Err((JSON_ERR_DEPTH_LIMIT, String::new()));
                }
                let close = if b == b'{' { b'}' } else { b']' };
                pos += 1;
                skip_ws(raw, &mut pos);
                if pos < raw.len() && raw[pos] == close {
                    pos += 1;
                    out.push(b);
                    out.push(close);
                    if stack.is_empty() {
                        break;
                    }
                    continue;
                }
                out.push(b);
                stack.push((b, 0));
                if pretty {
                    out.push(b'\n');
                    write_ind(out, base_depth + stack.len());
                }
            }
            b'}' | b']' => {
                let Some((_, count)) = stack.pop() else {
                    return Err((
                        JSON_ERR_UNSUPPORTED_VALUE,
                        "custom to_json() returned malformed JSON".to_string(),
                    ));
                };
                pos += 1;
                if count > 0 && pretty {
                    out.push(b'\n');
                    write_ind(out, base_depth + stack.len());
                }
                out.push(b);
                if stack.is_empty() {
                    break;
                }
            }
            b',' => {
                pos += 1;
                if let Some((_, count)) = stack.last_mut() {
                    *count += 1;
                }
                out.push(b',');
                if pretty {
                    out.push(b'\n');
                    write_ind(out, base_depth + stack.len());
                }
            }
            b':' => {
                pos += 1;
                if pretty {
                    out.extend_from_slice(b": ");
                } else {
                    out.push(b':');
                }
            }
            b'"' => {
                let start = pos;
                pos += 1;
                while pos < raw.len() {
                    match memchr::memchr2(b'"', b'\\', &raw[pos..]) {
                        Some(rel) => {
                            pos += rel;
                            if raw[pos] == b'"' {
                                pos += 1;
                                break;
                            }
                            pos = (pos + 2).min(raw.len());
                        }
                        None => {
                            return Err((
                                JSON_ERR_UNSUPPORTED_VALUE,
                                "custom to_json() returned unterminated JSON string".to_string(),
                            ));
                        }
                    }
                }
                out.extend_from_slice(&raw[start..pos]);
                if let Some((_, count)) = stack.last_mut()
                    && *count == 0
                {
                    *count = 1;
                }
                if stack.is_empty() {
                    break;
                }
            }
            _ => {
                let start = pos;
                while pos < raw.len()
                    && !matches!(
                        raw[pos],
                        b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'}' | b':'
                    )
                {
                    pos += 1;
                }
                out.extend_from_slice(&raw[start..pos]);
                if let Some((_, count)) = stack.last_mut()
                    && *count == 0
                {
                    *count = 1;
                }
                if stack.is_empty() {
                    break;
                }
            }
        }
    }
    skip_ws(raw, &mut pos);
    if !stack.is_empty() || pos < raw.len() {
        return Err((
            JSON_ERR_UNSUPPORTED_VALUE,
            "custom to_json() returned invalid JSON".to_string(),
        ));
    }
    Ok(())
}

fn write_scalar_value(vm: &Vm, val: TaggedValue, buf: &mut Vec<u8>) -> Result<(), (i64, String)> {
    if val.is_null() || val.is_unit() {
        buf.extend_from_slice(b"null");
        return Ok(());
    }
    if let Some(b) = val.as_bool() {
        buf.extend_from_slice(if b { b"true" } else { b"false" });
        return Ok(());
    }
    if let Some(ch) = val.as_char() {
        let mut utf8 = [0u8; 4];
        write_escaped_json_str(buf, ch.encode_utf8(&mut utf8).as_bytes());
        return Ok(());
    }
    if with_heap_string_str(val, |s| {
        write_escaped_json_str(buf, s.as_bytes());
    })
    .is_some()
    {
        return Ok(());
    }
    let is_f32_box = val.as_heap_ptr().is_some_and(|ptr| {
        // SAFETY: `val` is rooted in the active VM operation.
        let hdr = unsafe { ObjectHeader::from_payload_ptr(ptr) };
        hdr.is_ordinary() && hdr.type_index == Intrinsic::F32.type_index()
    });
    // SAFETY: `val` is rooted in the active VM operation; `Number::from_tagged` copies scalar bits.
    if let Some(num) = unsafe { Number::from_tagged(val) } {
        let mut itoa_buf = itoa::Buffer::new();
        match num {
            Number::I64(v) => buf.extend_from_slice(itoa_buf.format(v).as_bytes()),
            Number::U64(v) => buf.extend_from_slice(itoa_buf.format(v).as_bytes()),
            Number::I128(v) => buf.extend_from_slice(itoa_buf.format(v).as_bytes()),
            Number::U128(v) => buf.extend_from_slice(itoa_buf.format(v).as_bytes()),
            Number::F64(v) => {
                if !v.is_finite() {
                    return Err((
                        JSON_ERR_UNSUPPORTED_VALUE,
                        "non-finite float cannot be serialized to JSON".to_string(),
                    ));
                }
                if v == 0.0 {
                    buf.extend_from_slice(b"0.0");
                } else if is_f32_box {
                    let v32 = v as f32;
                    let _ = write!(buf, "{v32}");
                } else {
                    let _ = write!(buf, "{v}");
                }
            }
        }
        return Ok(());
    }
    let ty_name = vm
        .reflected_type(val)
        .ok()
        .and_then(|ty| vm.type_pool_read().display_name(ty))
        .unwrap_or_else(|| "<unknown>".to_string());
    Err((
        JSON_ERR_UNSUPPORTED_VALUE,
        format!("cannot serialize value of type {ty_name}"),
    ))
}

// ---------------------------------------------------------------------------
// Dynamic `parse` materialization and dynamic `stringify` traversal
// ---------------------------------------------------------------------------

fn materialize_dynamic_node(
    vm: &mut Vm,
    tape: &JsonTape,
    node_idx: usize,
) -> Result<TaggedValue, VmError> {
    match &tape.nodes[node_idx] {
        TapeNode::Null => Ok(TaggedValue::NULL),
        TapeNode::Bool(b) => Ok(TaggedValue::from_bool(*b)),
        TapeNode::Number {
            offset,
            len,
            is_float,
        } => {
            let slice = &tape.input[*offset as usize..(*offset as usize + *len as usize)];
            let token = std::str::from_utf8(slice).unwrap_or("0");
            // Per RFC 8259 / std.json contract: `-0` parses as `f64` (`-0.0`).
            if !*is_float
                && slice != b"-0"
                && let Ok(i) = token.parse::<i64>()
            {
                return vm.number_value(Number::I64(i));
            }
            let f = token.parse::<f64>().unwrap_or(0.0);
            vm.number_value(Number::F64(f))
        }
        TapeNode::Str(sref) => vm.alloc_string(tape.str_slice(*sref)),
        TapeNode::Array { start, len } => {
            let start = *start as usize;
            let len = *len as usize;
            let list = vm.new_list(len)?;
            let root_idx = vm.worker_roots().temporary_values.borrow().len();
            vm.with_temporary_roots(&[list], |vm| {
                for i in 0..len {
                    let child_node = tape.array_children[start + i] as usize;
                    let elem = materialize_dynamic_node(vm, tape, child_node)?;
                    let elem_root = vm.worker_roots().temporary_values.borrow().len();
                    vm.with_temporary_roots(&[elem], |vm| {
                        let cur_list = vm.worker_roots().temporary_values.borrow()[root_idx];
                        let cur_elem = vm.worker_roots().temporary_values.borrow()[elem_root];
                        vm.list_set(cur_list, TaggedValue::from_u64(i as u64), cur_elem)
                    })?;
                }
                Ok(vm.worker_roots().temporary_values.borrow()[root_idx])
            })
        }
        TapeNode::Object { start, len, .. } => {
            let start = *start as usize;
            let len = *len as usize;
            let map = vm.new_map(len)?;
            let map_root = vm.worker_roots().temporary_values.borrow().len();
            vm.with_temporary_roots(&[map], |vm| {
                for i in 0..len {
                    let entry = tape.object_entries[start + i];
                    let key_val = vm.alloc_string(tape.str_slice(entry.key))?;
                    let key_root = vm.worker_roots().temporary_values.borrow().len();
                    vm.with_temporary_roots(&[key_val], |vm| {
                        let child_val =
                            materialize_dynamic_node(vm, tape, entry.val_node as usize)?;
                        let val_root = vm.worker_roots().temporary_values.borrow().len();
                        vm.with_temporary_roots(&[child_val], |vm| {
                            let cur_map = vm.worker_roots().temporary_values.borrow()[map_root];
                            let cur_key = vm.worker_roots().temporary_values.borrow()[key_root];
                            let cur_val = vm.worker_roots().temporary_values.borrow()[val_root];
                            vm.collection_set(cur_map, cur_key, cur_val)
                        })
                    })?;
                }
                Ok(vm.worker_roots().temporary_values.borrow()[map_root])
            })
        }
    }
}

/// Pure host-side dynamic stringify: performs zero managed allocations during
/// traversal, so no GC safepoint can move objects while `ancestors` tracks raw bits.
fn stringify_dynamic_into(
    vm: &Vm,
    val: TaggedValue,
    pretty: bool,
    depth: usize,
    ancestors: &mut Vec<u64>,
    buf: &mut Vec<u8>,
) -> Result<(), (i64, String)> {
    if depth > MAX_JSON_DEPTH {
        return Err((JSON_ERR_DEPTH_LIMIT, String::new()));
    }
    if val.is_null() {
        buf.extend_from_slice(b"null");
        return Ok(());
    }
    if let Some(ptr) = val.as_heap_ptr() {
        // SAFETY: `val` is reachable from a rooted VM value, and this function
        // performs zero managed heap allocations so no GC can run mid-traversal.
        let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
        if header.is_ordinary() {
            let role = vm
                .type_pool_read()
                .checked_collection_role(header.type_index);
            match role {
                Ok(Some(CollectionRole::List)) => {
                    if depth + 1 > MAX_JSON_DEPTH {
                        return Err((JSON_ERR_DEPTH_LIMIT, String::new()));
                    }
                    if ancestors.contains(&val.raw()) {
                        return Err((JSON_ERR_CYCLE_DETECTED, String::new()));
                    }
                    let elems = vm.snapshot_list_elements(val).map_err(|_| {
                        (
                            JSON_ERR_UNSUPPORTED_VALUE,
                            "corrupt List layout".to_string(),
                        )
                    })?;
                    if elems.is_empty() {
                        buf.extend_from_slice(b"[]");
                        return Ok(());
                    }
                    ancestors.push(val.raw());
                    buf.push(b'[');
                    for (i, &elem) in elems.iter().enumerate() {
                        if i > 0 {
                            buf.push(b',');
                        }
                        if pretty {
                            buf.push(b'\n');
                            write_spaces(buf, (depth + 1) * 2);
                        }
                        stringify_dynamic_into(vm, elem, pretty, depth + 1, ancestors, buf)?;
                    }
                    ancestors.pop();
                    if pretty {
                        buf.push(b'\n');
                        write_spaces(buf, depth * 2);
                    }
                    buf.push(b']');
                    return Ok(());
                }
                Ok(Some(CollectionRole::Map)) => {
                    if depth + 1 > MAX_JSON_DEPTH {
                        return Err((JSON_ERR_DEPTH_LIMIT, String::new()));
                    }
                    if ancestors.contains(&val.raw()) {
                        return Err((JSON_ERR_CYCLE_DETECTED, String::new()));
                    }
                    let mut entries = {
                        let _guard = vm.stripe_read(val).map_err(|_| {
                            (JSON_ERR_UNSUPPORTED_VALUE, "corrupt Map layout".to_string())
                        })?;
                        let layout = vm.map_layout_unlocked(val).map_err(|_| {
                            (JSON_ERR_UNSUPPORTED_VALUE, "corrupt Map layout".to_string())
                        })?;
                        if layout.length == 0 {
                            buf.extend_from_slice(b"{}");
                            return Ok(());
                        }
                        layout.snapshot_entries().map_err(|_| {
                            (
                                JSON_ERR_UNSUPPORTED_VALUE,
                                "JSON object keys must be String".to_string(),
                            )
                        })?
                    };
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    ancestors.push(val.raw());
                    buf.push(b'{');
                    for (i, (k, v)) in entries.iter().enumerate() {
                        if i > 0 {
                            buf.push(b',');
                        }
                        if pretty {
                            buf.push(b'\n');
                            write_spaces(buf, (depth + 1) * 2);
                        }
                        write_escaped_json_str(buf, k.as_bytes());
                        if pretty {
                            buf.extend_from_slice(b": ");
                        } else {
                            buf.push(b':');
                        }
                        stringify_dynamic_into(vm, *v, pretty, depth + 1, ancestors, buf)?;
                    }
                    ancestors.pop();
                    if pretty {
                        buf.push(b'\n');
                        write_spaces(buf, depth * 2);
                    }
                    buf.push(b'}');
                    return Ok(());
                }
                _ => {}
            }
        }
    }
    if val.is_unit() {
        let ty_name = vm
            .reflected_type(val)
            .ok()
            .and_then(|ty| vm.type_pool_read().display_name(ty))
            .unwrap_or_else(|| "Unit".to_string());
        return Err((
            JSON_ERR_UNSUPPORTED_VALUE,
            format!("cannot serialize value of type {ty_name}"),
        ));
    }
    write_scalar_value(vm, val, buf)
}

#[inline]
fn write_spaces(buf: &mut Vec<u8>, count: usize) {
    const SPACES: &[u8; 64] = &[b' '; 64];
    let mut rem = count;
    while rem >= 64 {
        buf.extend_from_slice(SPACES);
        rem -= 64;
    }
    if rem > 0 {
        buf.extend_from_slice(&SPACES[..rem]);
    }
}

// ---------------------------------------------------------------------------
// `BuiltinCtx` entrypoints for IDs `670..=687`
// ---------------------------------------------------------------------------

impl BuiltinCtx<'_> {
    /// `670` `__json_parse_dynamic(text: String) -> (i64, i64, String, Any)`
    pub fn json_parse_dynamic(&mut self) -> Result<(), VmError> {
        self.require_arity(1)?;
        let input_val = self.argument_snapshot(0)?;
        let scan_res = with_heap_string_str(input_val, |s| JsonTape::scan(s.as_bytes()))
            .ok_or(VmError::TypeError)?;

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::I64.type_index(),
            Intrinsic::Str.type_index(),
            Intrinsic::Any.type_index(),
        ];

        match scan_res {
            Ok(tape) => {
                let parsed = materialize_dynamic_node(self.vm, &tape, 0)?;
                let parsed_root = self.vm.worker_roots().temporary_values.borrow().len();
                self.vm
                    .with_temporary_roots(&[parsed], |vm| {
                        let empty = vm.alloc_string("")?;
                        let empty_root = vm.worker_roots().temporary_values.borrow().len();
                        vm.with_temporary_roots(&[empty], |vm| {
                            let code_v = vm.number_value(Number::I64(JSON_OK))?;
                            let off_v = vm.number_value(Number::I64(0))?;
                            let msg_v = vm.worker_roots().temporary_values.borrow()[empty_root];
                            let val_v = vm.worker_roots().temporary_values.borrow()[parsed_root];
                            vm.alloc_tuple(&elem_types, &[code_v, off_v, msg_v, val_v])
                        })
                    })
                    .map(|tuple| self.write_return(tuple))
            }
            Err(err) => {
                let msg_val = self.vm.alloc_string(&err.message)?;
                let msg_root = self.vm.worker_roots().temporary_values.borrow().len();
                self.vm
                    .with_temporary_roots(&[msg_val], |vm| {
                        let code_v = vm.number_value(Number::I64(err.code))?;
                        let code_root = vm.worker_roots().temporary_values.borrow().len();
                        vm.with_temporary_roots(&[code_v], |vm| {
                            let off_v = vm.number_value(Number::I64(err.offset))?;
                            let c = vm.worker_roots().temporary_values.borrow()[code_root];
                            let m = vm.worker_roots().temporary_values.borrow()[msg_root];
                            vm.alloc_tuple(&elem_types, &[c, off_v, m, TaggedValue::NULL])
                        })
                    })
                    .map(|tuple| self.write_return(tuple))
            }
        }
    }

    /// `671` `__json_stringify_dynamic(value: Any, pretty: bool) -> (i64, String, String)`
    pub fn json_stringify_dynamic(&mut self) -> Result<(), VmError> {
        self.require_arity(2)?;
        let value = self.argument_snapshot(0)?;
        let pretty = self.arg_bool(1)?;

        let mut buf = Vec::with_capacity(128);
        let mut ancestors = Vec::new();
        let res = stringify_dynamic_into(self.vm, value, pretty, 0, &mut ancestors, &mut buf);

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::Str.type_index(),
            Intrinsic::Str.type_index(),
        ];

        let (code, msg_str, out_str) = match res {
            Ok(()) => {
                let out = String::from_utf8(buf).unwrap_or_default();
                (JSON_OK, String::new(), out)
            }
            Err((code, msg)) => (code, msg, String::new()),
        };

        let msg_v = self.vm.alloc_string(&msg_str)?;
        let msg_root = self.vm.worker_roots().temporary_values.borrow().len();
        self.vm
            .with_temporary_roots(&[msg_v], |vm| {
                let out_v = vm.alloc_string(&out_str)?;
                let out_root = vm.worker_roots().temporary_values.borrow().len();
                vm.with_temporary_roots(&[out_v], |vm| {
                    let code_v = vm.number_value(Number::I64(code))?;
                    let m = vm.worker_roots().temporary_values.borrow()[msg_root];
                    let o = vm.worker_roots().temporary_values.borrow()[out_root];
                    vm.alloc_tuple(&elem_types, &[code_v, m, o])
                })
            })
            .map(|tuple| self.write_return(tuple))
    }

    /// `672` `__json_tape_open(text: String) -> (i64, i64, String)`
    pub fn json_tape_open(&mut self) -> Result<(), VmError> {
        self.require_arity(1)?;
        let input_val = self.argument_snapshot(0)?;
        let scan_res = with_heap_string_str(input_val, |s| JsonTape::scan(s.as_bytes()))
            .ok_or(VmError::TypeError)?;

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::I64.type_index(),
            Intrinsic::Str.type_index(),
        ];

        let (code, handle_or_off, msg) = match scan_res {
            Ok(tape) => {
                let mut json = self.vm.shared.json.lock();
                let id = json.next_tape_id;
                json.next_tape_id = id.wrapping_add(1).max(1);
                json.tapes.insert(id, tape);
                (JSON_OK, id, String::new())
            }
            Err(err) => (err.code, err.offset, err.message),
        };

        let msg_v = self.vm.alloc_string(&msg)?;
        let msg_root = self.vm.worker_roots().temporary_values.borrow().len();
        self.vm
            .with_temporary_roots(&[msg_v], |vm| {
                let c_v = vm.number_value(Number::I64(code))?;
                let c_root = vm.worker_roots().temporary_values.borrow().len();
                vm.with_temporary_roots(&[c_v], |vm| {
                    let h_v = vm.number_value(Number::I64(handle_or_off))?;
                    let c = vm.worker_roots().temporary_values.borrow()[c_root];
                    let m = vm.worker_roots().temporary_values.borrow()[msg_root];
                    vm.alloc_tuple(&elem_types, &[c, h_v, m])
                })
            })
            .map(|tuple| self.write_return(tuple))
    }

    /// `673` `__json_tape_close(tape: i64) -> Unit`
    pub fn json_tape_close(&mut self) -> Result<(), VmError> {
        self.require_arity(1)?;
        let handle = self.arg_i64(0)?;
        self.vm.shared.json.lock().tapes.remove(&handle);
        self.return_unit();
        Ok(())
    }

    /// `674` `__json_tape_kind(tape: i64, node: i64) -> i64`
    pub fn json_tape_kind(&mut self) -> Result<(), VmError> {
        self.require_arity(2)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let kind = if node < 0 {
            TAPE_KIND_INVALID
        } else {
            let json = self.vm.shared.json.lock();
            if let Some(tape) = json.tapes.get(&handle) {
                tape.node_kind_tag(node as usize)
            } else {
                TAPE_KIND_INVALID
            }
        };
        let v = self.vm.number_value(Number::I64(kind))?;
        self.write_return(v);
        Ok(())
    }

    /// `675` `__json_tape_obj_get(tape: i64, node: i64, key: String) -> i64`
    pub fn json_tape_obj_get(&mut self) -> Result<(), VmError> {
        self.require_arity(3)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let key_val = self.argument_snapshot(2)?;
        if node < 0 {
            let v = self.vm.number_value(Number::I64(-1))?;
            self.write_return(v);
            return Ok(());
        }
        let found = with_heap_string_str(key_val, |k| {
            self.vm
                .shared
                .json
                .lock()
                .tapes
                .get_mut(&handle)
                .and_then(|tape| tape.object_lookup(node as usize, k.as_bytes()))
                .map_or(-1i64, i64::from)
        })
        .ok_or(VmError::TypeError)?;
        let v = self.vm.number_value(Number::I64(found))?;
        self.write_return(v);
        Ok(())
    }

    /// `676` `__json_tape_obj_unknown(tape: i64, node: i64, known_keys: Any) -> ?String`
    pub fn json_tape_obj_unknown(&mut self) -> Result<(), VmError> {
        self.require_arity(3)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let known_val = self.argument_snapshot(2)?;

        let known_slots: Vec<TaggedValue> = if known_val.is_unit() || known_val.is_null() {
            Vec::new()
        } else if let Ok(tuple_elems) = self.vm.tuple_values(known_val) {
            tuple_elems
        } else if let Ok(list_elems) = self.vm.snapshot_list_elements(known_val) {
            list_elems
        } else {
            return Err(VmError::TypeError);
        };

        let unknown: Option<String> = (|| {
            if node < 0 {
                return None;
            }
            let json = self.vm.shared.json.lock();
            let tape = json.tapes.get(&handle)?;
            let TapeNode::Object { start, len, .. } = tape.nodes.get(node as usize)? else {
                return None;
            };
            let start = *start as usize;
            let len = *len as usize;
            'entry_loop: for i in 0..len {
                let key_bytes = tape.str_bytes(tape.object_entries[start + i].key);
                for &known_tv in &known_slots {
                    let matched = with_heap_string_str(known_tv, |ks| ks.as_bytes() == key_bytes)
                        .unwrap_or(false);
                    if matched {
                        continue 'entry_loop;
                    }
                }
                return Some(
                    tape.str_slice(tape.object_entries[start + i].key)
                        .to_owned(),
                );
            }
            None
        })();

        match unknown {
            Some(unk) => {
                let s = self.vm.alloc_string(&unk)?;
                self.write_return(s);
            }
            None => self.return_null(),
        }
        Ok(())
    }

    /// `677` `__json_tape_obj_single_variant(tape: i64, node: i64) -> (i64, String)`
    pub fn json_tape_obj_single_variant(&mut self) -> Result<(), VmError> {
        self.require_arity(2)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;

        let single: Option<(i64, String)> = (|| {
            if node < 0 {
                return None;
            }
            let json = self.vm.shared.json.lock();
            let tape = json.tapes.get(&handle)?;
            let TapeNode::Object { start, len, .. } = tape.nodes.get(node as usize)? else {
                return None;
            };
            if *len != 1 {
                return None;
            }
            let entry = tape.object_entries[*start as usize];
            Some((
                i64::from(entry.val_node),
                tape.str_slice(entry.key).to_owned(),
            ))
        })();

        let elem_types = [Intrinsic::I64.type_index(), Intrinsic::Str.type_index()];
        let (child_node, vname) = single.unwrap_or((-1, String::new()));
        let s_v = self.vm.alloc_string(&vname)?;
        let s_root = self.vm.worker_roots().temporary_values.borrow().len();
        self.vm
            .with_temporary_roots(&[s_v], |vm| {
                let n_v = vm.number_value(Number::I64(child_node))?;
                let s = vm.worker_roots().temporary_values.borrow()[s_root];
                vm.alloc_tuple(&elem_types, &[n_v, s])
            })
            .map(|tuple| self.write_return(tuple))
    }

    /// `678` `__json_tape_arr_len(tape: i64, node: i64) -> i64`
    pub fn json_tape_arr_len(&mut self) -> Result<(), VmError> {
        self.require_arity(2)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let len = {
            let json = self.vm.shared.json.lock();
            if node >= 0
                && let Some(tape) = json.tapes.get(&handle)
                && let Some(TapeNode::Array { len, .. }) = tape.nodes.get(node as usize)
            {
                i64::from(*len)
            } else {
                -1
            }
        };
        let v = self.vm.number_value(Number::I64(len))?;
        self.write_return(v);
        Ok(())
    }

    /// `679` `__json_tape_arr_at(tape: i64, node: i64, idx: i64) -> i64`
    pub fn json_tape_arr_at(&mut self) -> Result<(), VmError> {
        self.require_arity(3)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let idx = self.arg_i64(2)?;
        let child = {
            let json = self.vm.shared.json.lock();
            if node >= 0
                && idx >= 0
                && let Some(tape) = json.tapes.get(&handle)
                && let Some(TapeNode::Array { start, len }) = tape.nodes.get(node as usize)
                && (idx as u64) < u64::from(*len)
            {
                i64::from(tape.array_children[*start as usize + idx as usize])
            } else {
                -1
            }
        };
        let v = self.vm.number_value(Number::I64(child))?;
        self.write_return(v);
        Ok(())
    }

    /// `680` `__json_tape_read_bool(tape: i64, node: i64) -> (i64, bool, String)`
    pub fn json_tape_read_bool(&mut self) -> Result<(), VmError> {
        self.require_arity(2)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;

        let (code, flag, kind_name) = {
            let json = self.vm.shared.json.lock();
            if node >= 0
                && let Some(tape) = json.tapes.get(&handle)
            {
                match tape.nodes.get(node as usize) {
                    Some(TapeNode::Bool(b)) => (JSON_OK, *b, ""),
                    _ => (
                        JSON_ERR_TYPE_MISMATCH,
                        false,
                        tape.node_kind_name(node as usize),
                    ),
                }
            } else {
                (JSON_ERR_TYPE_MISMATCH, false, "invalid")
            }
        };

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::Bool.type_index(),
            Intrinsic::Str.type_index(),
        ];
        let k_v = self.vm.alloc_string(kind_name)?;
        let k_root = self.vm.worker_roots().temporary_values.borrow().len();
        self.vm
            .with_temporary_roots(&[k_v], |vm| {
                let c_v = vm.number_value(Number::I64(code))?;
                let k = vm.worker_roots().temporary_values.borrow()[k_root];
                vm.alloc_tuple(&elem_types, &[c_v, TaggedValue::from_bool(flag), k])
            })
            .map(|tuple| self.write_return(tuple))
    }

    /// `681` `__json_tape_read_int(tape: i64, node: i64, width_tag: i64) -> (i64, Any, String)`
    pub fn json_tape_read_int(&mut self) -> Result<(), VmError> {
        self.require_arity(3)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let width_tag = self.arg_i64(2)?;
        let target_kind = width_tag_intrinsic(width_tag).ok_or(VmError::TypeError)?;

        enum IntOutcome {
            Ok(ParsedInt),
            Mismatch(&'static str),
            OutOfRange(String),
        }

        let outcome = {
            let json = self.vm.shared.json.lock();
            if node >= 0
                && let Some(tape) = json.tapes.get(&handle)
            {
                match tape.nodes.get(node as usize) {
                    Some(TapeNode::Number {
                        offset,
                        len,
                        is_float: false,
                    }) => {
                        let slice =
                            &tape.input[*offset as usize..(*offset as usize + *len as usize)];
                        match parse_exact_int(slice, target_kind) {
                            Ok(parsed) => IntOutcome::Ok(parsed),
                            Err(()) => IntOutcome::OutOfRange(
                                std::str::from_utf8(slice).unwrap_or("").to_owned(),
                            ),
                        }
                    }
                    Some(TapeNode::Number { is_float: true, .. }) => IntOutcome::Mismatch("float"),
                    _ => IntOutcome::Mismatch(tape.node_kind_name(node as usize)),
                }
            } else {
                IntOutcome::Mismatch("invalid")
            }
        };

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::Any.type_index(),
            Intrinsic::Str.type_index(),
        ];

        match outcome {
            IntOutcome::Ok(parsed) => {
                let int_v = alloc_parsed_int(self.vm, target_kind, parsed)?;
                let int_root = self.vm.worker_roots().temporary_values.borrow().len();
                self.vm
                    .with_temporary_roots(&[int_v], |vm| {
                        let empty_v = vm.alloc_string("")?;
                        let empty_root = vm.worker_roots().temporary_values.borrow().len();
                        vm.with_temporary_roots(&[empty_v], |vm| {
                            let c_v = vm.number_value(Number::I64(JSON_OK))?;
                            let iv = vm.worker_roots().temporary_values.borrow()[int_root];
                            let ev = vm.worker_roots().temporary_values.borrow()[empty_root];
                            vm.alloc_tuple(&elem_types, &[c_v, iv, ev])
                        })
                    })
                    .map(|tuple| self.write_return(tuple))
            }
            IntOutcome::Mismatch(actual) => {
                let k_v = self.vm.alloc_string(actual)?;
                let k_root = self.vm.worker_roots().temporary_values.borrow().len();
                self.vm
                    .with_temporary_roots(&[k_v], |vm| {
                        let zero_v = alloc_parsed_int(
                            vm,
                            target_kind,
                            if matches!(
                                target_kind,
                                Intrinsic::U8
                                    | Intrinsic::U16
                                    | Intrinsic::U32
                                    | Intrinsic::U64
                                    | Intrinsic::U128
                                    | Intrinsic::Usize
                            ) {
                                ParsedInt::Unsigned(0)
                            } else {
                                ParsedInt::Signed(0)
                            },
                        )?;
                        let z_root = vm.worker_roots().temporary_values.borrow().len();
                        vm.with_temporary_roots(&[zero_v], |vm| {
                            let c_v = vm.number_value(Number::I64(JSON_ERR_TYPE_MISMATCH))?;
                            let zv = vm.worker_roots().temporary_values.borrow()[z_root];
                            let kv = vm.worker_roots().temporary_values.borrow()[k_root];
                            vm.alloc_tuple(&elem_types, &[c_v, zv, kv])
                        })
                    })
                    .map(|tuple| self.write_return(tuple))
            }
            IntOutcome::OutOfRange(raw_tok) => {
                let t_v = self.vm.alloc_string(&raw_tok)?;
                let t_root = self.vm.worker_roots().temporary_values.borrow().len();
                self.vm
                    .with_temporary_roots(&[t_v], |vm| {
                        let zero_v = alloc_parsed_int(
                            vm,
                            target_kind,
                            if matches!(
                                target_kind,
                                Intrinsic::U8
                                    | Intrinsic::U16
                                    | Intrinsic::U32
                                    | Intrinsic::U64
                                    | Intrinsic::U128
                                    | Intrinsic::Usize
                            ) {
                                ParsedInt::Unsigned(0)
                            } else {
                                ParsedInt::Signed(0)
                            },
                        )?;
                        let z_root = vm.worker_roots().temporary_values.borrow().len();
                        vm.with_temporary_roots(&[zero_v], |vm| {
                            let c_v = vm.number_value(Number::I64(JSON_ERR_INT_OUT_OF_RANGE))?;
                            let zv = vm.worker_roots().temporary_values.borrow()[z_root];
                            let tv = vm.worker_roots().temporary_values.borrow()[t_root];
                            vm.alloc_tuple(&elem_types, &[c_v, zv, tv])
                        })
                    })
                    .map(|tuple| self.write_return(tuple))
            }
        }
    }

    /// `682` `__json_tape_read_float(tape: i64, node: i64, is_f32: bool) -> (i64, Any, String)`
    pub fn json_tape_read_float(&mut self) -> Result<(), VmError> {
        self.require_arity(3)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let is_f32 = self.arg_bool(2)?;
        let target_kind = if is_f32 {
            Intrinsic::F32
        } else {
            Intrinsic::F64
        };

        enum FloatOutcome {
            Ok(f64),
            Mismatch(&'static str),
            OutOfRange(String),
        }

        let outcome = {
            let json = self.vm.shared.json.lock();
            if node >= 0
                && let Some(tape) = json.tapes.get(&handle)
            {
                match tape.nodes.get(node as usize) {
                    Some(TapeNode::Number { offset, len, .. }) => {
                        let slice =
                            &tape.input[*offset as usize..(*offset as usize + *len as usize)];
                        let tok = std::str::from_utf8(slice).unwrap_or("0");
                        let v64 = tok.parse::<f64>().unwrap_or(f64::NAN);
                        if !v64.is_finite() {
                            FloatOutcome::OutOfRange(tok.to_owned())
                        } else if is_f32 {
                            let v32 = v64 as f32;
                            if !v32.is_finite() {
                                FloatOutcome::OutOfRange(tok.to_owned())
                            } else {
                                FloatOutcome::Ok(f64::from(v32))
                            }
                        } else {
                            FloatOutcome::Ok(v64)
                        }
                    }
                    _ => FloatOutcome::Mismatch(tape.node_kind_name(node as usize)),
                }
            } else {
                FloatOutcome::Mismatch("invalid")
            }
        };

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::Any.type_index(),
            Intrinsic::Str.type_index(),
        ];

        let (code, fval, info) = match outcome {
            FloatOutcome::Ok(v) => (JSON_OK, v, String::new()),
            FloatOutcome::Mismatch(actual) => (JSON_ERR_TYPE_MISMATCH, 0.0, actual.to_owned()),
            FloatOutcome::OutOfRange(raw) => (JSON_ERR_INT_OUT_OF_RANGE, 0.0, raw),
        };

        let flt_v = self.vm.typed_number_value(Number::F64(fval), target_kind)?;
        let flt_root = self.vm.worker_roots().temporary_values.borrow().len();
        self.vm
            .with_temporary_roots(&[flt_v], |vm| {
                let s_v = vm.alloc_string(&info)?;
                let s_root = vm.worker_roots().temporary_values.borrow().len();
                vm.with_temporary_roots(&[s_v], |vm| {
                    let c_v = vm.number_value(Number::I64(code))?;
                    let fv = vm.worker_roots().temporary_values.borrow()[flt_root];
                    let sv = vm.worker_roots().temporary_values.borrow()[s_root];
                    vm.alloc_tuple(&elem_types, &[c_v, fv, sv])
                })
            })
            .map(|tuple| self.write_return(tuple))
    }

    /// `683` `__json_tape_read_str(tape: i64, node: i64, mode: Any) -> (i64, Any, String)`
    /// `mode`: `false` = String, `true` = char, `2` (int) = compact raw JSON of subtree `node`.
    pub fn json_tape_read_str(&mut self) -> Result<(), VmError> {
        self.require_arity(3)?;
        let handle = self.arg_i64(0)?;
        let node = self.arg_i64(1)?;
        let mode_val = self.argument_snapshot(2)?;
        let (want_char, want_raw_json) = if let Some(b) = mode_val.as_bool() {
            (b, false)
        } else if self.arg_i64(2)? == 2 {
            (false, true)
        } else {
            return Err(VmError::TypeError);
        };

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::Any.type_index(),
            Intrinsic::Str.type_index(),
        ];

        if want_raw_json {
            let raw_res: Result<String, &'static str> = {
                let json = self.vm.shared.json.lock();
                if node >= 0
                    && let Some(tape) = json.tapes.get(&handle)
                    && (node as usize) < tape.nodes.len()
                {
                    let mut out = Vec::new();
                    write_tape_node_compact(tape, node as usize, &mut out);
                    Ok(String::from_utf8(out).unwrap_or_default())
                } else {
                    Err("invalid")
                }
            };
            let (code, val_str, info_str) = match raw_res {
                Ok(s) => (JSON_OK, s, ""),
                Err(actual) => (JSON_ERR_TYPE_MISMATCH, String::new(), actual),
            };
            let v_s = self.vm.alloc_string(&val_str)?;
            let v_root = self.vm.worker_roots().temporary_values.borrow().len();
            return self
                .vm
                .with_temporary_roots(&[v_s], |vm| {
                    let i_s = vm.alloc_string(info_str)?;
                    let i_root = vm.worker_roots().temporary_values.borrow().len();
                    vm.with_temporary_roots(&[i_s], |vm| {
                        let c_v = vm.number_value(Number::I64(code))?;
                        let vs = vm.worker_roots().temporary_values.borrow()[v_root];
                        let is = vm.worker_roots().temporary_values.borrow()[i_root];
                        vm.alloc_tuple(&elem_types, &[c_v, vs, is])
                    })
                })
                .map(|tuple| self.write_return(tuple));
        }

        if want_char {
            let char_res: Result<char, &'static str> = {
                let json = self.vm.shared.json.lock();
                if node >= 0
                    && let Some(tape) = json.tapes.get(&handle)
                {
                    match tape.nodes.get(node as usize) {
                        Some(TapeNode::Str(sref)) => {
                            let s = tape.str_slice(*sref);
                            let mut chars = s.chars();
                            match (chars.next(), chars.next()) {
                                (Some(ch), None) => Ok(ch),
                                (None, _) => Err("empty string"),
                                _ => Err("multi-character string"),
                            }
                        }
                        _ => Err(tape.node_kind_name(node as usize)),
                    }
                } else {
                    Err("invalid")
                }
            };

            let (code, ch_val, info) = match char_res {
                Ok(ch) => (JSON_OK, TaggedValue::from_char(ch), ""),
                Err(actual) => (JSON_ERR_TYPE_MISMATCH, TaggedValue::from_char('\0'), actual),
            };
            let s_v = self.vm.alloc_string(info)?;
            let s_root = self.vm.worker_roots().temporary_values.borrow().len();
            return self
                .vm
                .with_temporary_roots(&[s_v], |vm| {
                    let c_v = vm.number_value(Number::I64(code))?;
                    let sv = vm.worker_roots().temporary_values.borrow()[s_root];
                    vm.alloc_tuple(&elem_types, &[c_v, ch_val, sv])
                })
                .map(|tuple| self.write_return(tuple));
        }

        let str_res: Result<String, &'static str> = {
            let json = self.vm.shared.json.lock();
            if node >= 0
                && let Some(tape) = json.tapes.get(&handle)
            {
                match tape.nodes.get(node as usize) {
                    Some(TapeNode::Str(sref)) => Ok(tape.str_slice(*sref).to_owned()),
                    _ => Err(tape.node_kind_name(node as usize)),
                }
            } else {
                Err("invalid")
            }
        };

        let (code, val_str, info_str) = match str_res {
            Ok(s) => (JSON_OK, s, ""),
            Err(actual) => (JSON_ERR_TYPE_MISMATCH, String::new(), actual),
        };
        let v_s = self.vm.alloc_string(&val_str)?;
        let v_root = self.vm.worker_roots().temporary_values.borrow().len();
        self.vm
            .with_temporary_roots(&[v_s], |vm| {
                let i_s = vm.alloc_string(info_str)?;
                let i_root = vm.worker_roots().temporary_values.borrow().len();
                vm.with_temporary_roots(&[i_s], |vm| {
                    let c_v = vm.number_value(Number::I64(code))?;
                    let vs = vm.worker_roots().temporary_values.borrow()[v_root];
                    let is = vm.worker_roots().temporary_values.borrow()[i_root];
                    vm.alloc_tuple(&elem_types, &[c_v, vs, is])
                })
            })
            .map(|tuple| self.write_return(tuple))
    }

    /// `684` `__json_writer_new(indent: i64) -> i64`
    pub fn json_writer_new(&mut self) -> Result<(), VmError> {
        self.require_arity(1)?;
        let indent = self.arg_i64(0)?;
        let id = {
            let mut json = self.vm.shared.json.lock();
            let id = json.next_writer_id;
            json.next_writer_id = id.wrapping_add(1).max(1);
            json.writers.insert(id, JsonWriter::new(indent));
            id
        };
        self.vm
            .roots()
            .json_writer_ancestors
            .lock()
            .insert(id, Vec::new());
        let v = self.vm.number_value(Number::I64(id))?;
        self.write_return(v);
        Ok(())
    }

    /// `685` `__json_writer_op(writer: i64, op: i64, arg: Any) -> i64`
    pub fn json_writer_op(&mut self) -> Result<(), VmError> {
        self.require_arity(3)?;
        let handle = self.arg_i64(0)?;
        let op = self.arg_i64(1)?;
        let arg = self.argument_snapshot(2)?;

        let status = match op {
            WRITER_OP_BEGIN_OBJ | WRITER_OP_BEGIN_ARR => {
                let is_tracked = arg.is_heap() && !arg.is_null() && !arg.is_unit();
                let cycle = is_tracked
                    && self
                        .vm
                        .roots()
                        .json_writer_ancestors
                        .lock()
                        .get(&handle)
                        .is_some_and(|stack| stack.contains(&arg));
                let st = {
                    let mut json = self.vm.shared.json.lock();
                    if let Some(writer) = json.writers.get_mut(&handle) {
                        if writer.err.is_some() {
                            writer.status()
                        } else if cycle {
                            writer.set_err(JSON_ERR_CYCLE_DETECTED, "")
                        } else if op == WRITER_OP_BEGIN_OBJ {
                            writer.begin_obj()
                        } else {
                            writer.begin_arr()
                        }
                    } else {
                        JSON_ERR_UNSUPPORTED_VALUE
                    }
                };
                if st == JSON_OK
                    && let Some(stack) = self
                        .vm
                        .roots()
                        .json_writer_ancestors
                        .lock()
                        .get_mut(&handle)
                {
                    stack.push(if is_tracked { arg } else { TaggedValue::NULL });
                }
                st
            }
            WRITER_OP_END_OBJ | WRITER_OP_END_ARR => {
                let mut should_pop = false;
                let st = {
                    let mut json = self.vm.shared.json.lock();
                    if let Some(writer) = json.writers.get_mut(&handle) {
                        if writer.err.is_some() {
                            writer.status()
                        } else {
                            should_pop = true;
                            if op == WRITER_OP_END_OBJ {
                                writer.end_obj()
                            } else {
                                writer.end_arr()
                            }
                        }
                    } else {
                        JSON_ERR_UNSUPPORTED_VALUE
                    }
                };
                if should_pop
                    && let Some(stack) = self
                        .vm
                        .roots()
                        .json_writer_ancestors
                        .lock()
                        .get_mut(&handle)
                {
                    stack.pop();
                }
                st
            }
            WRITER_OP_FIELD_KEY => {
                let key = heap_string_to_owned(arg).ok_or(VmError::TypeError)?;
                let mut json = self.vm.shared.json.lock();
                if let Some(writer) = json.writers.get_mut(&handle) {
                    writer.write_field_key(&key)
                } else {
                    JSON_ERR_UNSUPPORTED_VALUE
                }
            }
            WRITER_OP_SCALAR => {
                let already_err = self
                    .vm
                    .shared
                    .json
                    .lock()
                    .writers
                    .get(&handle)
                    .map_or(Some(JSON_ERR_UNSUPPORTED_VALUE), |w| {
                        w.err.as_ref().map(|(c, _)| *c)
                    });
                if let Some(code) = already_err {
                    code
                } else {
                    let mut tmp = Vec::with_capacity(32);
                    match write_scalar_value(self.vm, arg, &mut tmp) {
                        Ok(()) => {
                            let mut json = self.vm.shared.json.lock();
                            if let Some(writer) = json.writers.get_mut(&handle) {
                                writer.before_value();
                                writer.buf.extend_from_slice(&tmp);
                                JSON_OK
                            } else {
                                JSON_ERR_UNSUPPORTED_VALUE
                            }
                        }
                        Err((code, msg)) => {
                            let mut json = self.vm.shared.json.lock();
                            if let Some(writer) = json.writers.get_mut(&handle) {
                                writer.set_err(code, msg)
                            } else {
                                JSON_ERR_UNSUPPORTED_VALUE
                            }
                        }
                    }
                }
            }
            WRITER_OP_RAW_JSON => {
                let raw = heap_string_to_owned(arg).ok_or(VmError::TypeError)?;
                let mut json = self.vm.shared.json.lock();
                if let Some(writer) = json.writers.get_mut(&handle) {
                    writer.write_raw_json(raw.as_bytes())
                } else {
                    JSON_ERR_UNSUPPORTED_VALUE
                }
            }
            WRITER_OP_REFORMAT_JSON => {
                let raw = heap_string_to_owned(arg).ok_or(VmError::TypeError)?;
                let mut json = self.vm.shared.json.lock();
                if let Some(writer) = json.writers.get_mut(&handle) {
                    if writer.err.is_some() {
                        writer.status()
                    } else {
                        match reformat_json_into(raw.as_bytes(), &mut writer.buf, writer.indent, 0)
                        {
                            Ok(()) => JSON_OK,
                            Err((code, msg)) => writer.set_err(code, msg),
                        }
                    }
                } else {
                    JSON_ERR_UNSUPPORTED_VALUE
                }
            }
            _ => return Err(VmError::TypeError),
        };

        let v = self.vm.number_value(Number::I64(status))?;
        self.write_return(v);
        Ok(())
    }

    /// `686` `__json_writer_finish(writer: i64) -> (i64, String, String)`
    pub fn json_writer_finish(&mut self) -> Result<(), VmError> {
        self.require_arity(1)?;
        let handle = self.arg_i64(0)?;
        self.vm.roots().json_writer_ancestors.lock().remove(&handle);
        let removed = self.vm.shared.json.lock().writers.remove(&handle);
        let (code, msg, out) = match removed {
            Some(writer) => match writer.err {
                Some((code, msg)) => (code, msg, String::new()),
                None => (
                    JSON_OK,
                    String::new(),
                    String::from_utf8(writer.buf).unwrap_or_default(),
                ),
            },
            None => (
                JSON_ERR_UNSUPPORTED_VALUE,
                "invalid JSON writer handle".to_string(),
                String::new(),
            ),
        };

        let elem_types = [
            Intrinsic::I64.type_index(),
            Intrinsic::Str.type_index(),
            Intrinsic::Str.type_index(),
        ];
        let m_v = self.vm.alloc_string(&msg)?;
        let m_root = self.vm.worker_roots().temporary_values.borrow().len();
        self.vm
            .with_temporary_roots(&[m_v], |vm| {
                let o_v = vm.alloc_string(&out)?;
                let o_root = vm.worker_roots().temporary_values.borrow().len();
                vm.with_temporary_roots(&[o_v], |vm| {
                    let c_v = vm.number_value(Number::I64(code))?;
                    let mv = vm.worker_roots().temporary_values.borrow()[m_root];
                    let ov = vm.worker_roots().temporary_values.borrow()[o_root];
                    vm.alloc_tuple(&elem_types, &[c_v, mv, ov])
                })
            })
            .map(|tuple| self.write_return(tuple))
    }

    /// `687` `__json_writer_close(writer: i64) -> Unit`
    pub fn json_writer_close(&mut self) -> Result<(), VmError> {
        self.require_arity(1)?;
        let handle = self.arg_i64(0)?;
        self.vm.shared.json.lock().writers.remove(&handle);
        self.vm.roots().json_writer_ancestors.lock().remove(&handle);
        self.return_unit();
        Ok(())
    }
}

fn write_tape_node_compact(tape: &JsonTape, idx: usize, out: &mut Vec<u8>) {
    let Some(node) = tape.nodes.get(idx) else {
        return;
    };
    match *node {
        TapeNode::Null => out.extend_from_slice(b"null"),
        TapeNode::Bool(true) => out.extend_from_slice(b"true"),
        TapeNode::Bool(false) => out.extend_from_slice(b"false"),
        TapeNode::Number { offset, len, .. } => {
            out.extend_from_slice(&tape.input[offset as usize..(offset as usize + len as usize)]);
        }
        TapeNode::Str(sref) => {
            write_escaped_json_str(out, tape.str_bytes(sref));
        }
        TapeNode::Array { start, len } => {
            out.push(b'[');
            for i in 0..len {
                if i > 0 {
                    out.push(b',');
                }
                let elem_node = tape.array_children[(start + i) as usize] as usize;
                write_tape_node_compact(tape, elem_node, out);
            }
            out.push(b']');
        }
        TapeNode::Object { start, len, .. } => {
            out.push(b'{');
            for i in 0..len {
                if i > 0 {
                    out.push(b',');
                }
                let entry = &tape.object_entries[(start + i) as usize];
                write_escaped_json_str(out, tape.str_bytes(entry.key));
                out.push(b':');
                write_tape_node_compact(tape, entry.val_node as usize, out);
            }
            out.push(b'}');
        }
    }
}
