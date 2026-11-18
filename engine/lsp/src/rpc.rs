//! JSON-RPC 2.0 `Content-Length` stdio framing for the Language Server Protocol.

use std::io::{self, BufRead, Read, Write};

use crate::json::JsonValue;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const SERVER_NOT_INITIALIZED: i64 = -32002;
pub const REQUEST_CANCELLED: i64 = -32800;

pub const ERR_PARSE_ERROR: i64 = PARSE_ERROR;
pub const ERR_INVALID_REQUEST: i64 = INVALID_REQUEST;
pub const ERR_METHOD_NOT_FOUND: i64 = METHOD_NOT_FOUND;
pub const ERR_INVALID_PARAMS: i64 = INVALID_PARAMS;

/// Hard upper bound on a single JSON-RPC message payload (64 MiB).
pub const MAX_RPC_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Maximum bytes permitted per HTTP-style header line (8 KiB).
const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
/// Maximum number of header lines per frame.
const MAX_HEADER_COUNT: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestId {
    Int(i64),
    Str(String),
    Null,
}

impl RequestId {
    pub fn from_json(val: &JsonValue) -> Option<Self> {
        match val {
            JsonValue::Int(n) => Some(RequestId::Int(*n)),
            JsonValue::Str(s) => Some(RequestId::Str(s.clone())),
            JsonValue::Null => Some(RequestId::Null),
            _ => None,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        match self {
            RequestId::Int(n) => JsonValue::Int(*n),
            RequestId::Str(s) => JsonValue::Str(s.clone()),
            RequestId::Null => JsonValue::Null,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RpcMessage {
    Request {
        id: RequestId,
        method: String,
        params: JsonValue,
    },
    Notification {
        method: String,
        params: JsonValue,
    },
    Response {
        id: RequestId,
        result: Option<JsonValue>,
        error: Option<RpcError>,
    },
}

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<JsonValue>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut fields = vec![
            ("code".to_owned(), JsonValue::Int(self.code)),
            ("message".to_owned(), JsonValue::Str(self.message.clone())),
        ];
        if let Some(data) = &self.data {
            fields.push(("data".to_owned(), data.clone()));
        }
        JsonValue::Object(fields)
    }
}

impl RpcMessage {
    pub fn from_json(value: JsonValue) -> Result<Self, RpcError> {
        let _ = value.as_object().ok_or_else(|| {
            RpcError::new(INVALID_REQUEST, "JSON-RPC payload must be a JSON object")
        })?;
        let has_id_field = value.get("id").is_some();
        let id = match value.get("id") {
            Some(raw_id) => Some(RequestId::from_json(raw_id).ok_or_else(|| {
                RpcError::new(
                    INVALID_REQUEST,
                    "JSON-RPC 'id' must be integer, string, or null",
                )
            })?),
            None => None,
        };
        if let Some(method_val) = value.get("method") {
            let method = method_val
                .as_str()
                .ok_or_else(|| RpcError::new(INVALID_REQUEST, "JSON-RPC 'method' must be string"))?
                .to_owned();
            let params = value.get("params").cloned().unwrap_or(JsonValue::Null);
            if has_id_field {
                Ok(RpcMessage::Request {
                    id: id.unwrap_or(RequestId::Null),
                    method,
                    params,
                })
            } else {
                Ok(RpcMessage::Notification { method, params })
            }
        } else if let Some(id) = id {
            let result = value.get("result").cloned();
            let error = value.get("error").map(|err| RpcError {
                code: err.get_i64("code").unwrap_or(INTERNAL_ERROR),
                message: err
                    .get_str("message")
                    .unwrap_or("unknown RPC error")
                    .to_owned(),
                data: err.get("data").cloned(),
            });
            Ok(RpcMessage::Response { id, result, error })
        } else {
            Err(RpcError::new(
                INVALID_REQUEST,
                "JSON-RPC object missing 'method' or 'id'",
            ))
        }
    }

    pub fn ok_response(id: RequestId, result: JsonValue) -> JsonValue {
        JsonValue::object([
            ("jsonrpc", JsonValue::from("2.0")),
            ("id", id.to_json()),
            ("result", result),
        ])
    }

    pub fn error_response(id: RequestId, error: RpcError) -> JsonValue {
        JsonValue::object([
            ("jsonrpc", JsonValue::from("2.0")),
            ("id", id.to_json()),
            ("error", error.to_json()),
        ])
    }

    pub fn notification(method: &str, params: JsonValue) -> JsonValue {
        JsonValue::object([
            ("jsonrpc", JsonValue::from("2.0")),
            ("method", JsonValue::from(method)),
            ("params", params),
        ])
    }
}

fn read_bounded_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<usize> {
    buf.clear();
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(nl_pos) = available.iter().position(|&b| b == b'\n') {
            let take_len = nl_pos + 1;
            if total + take_len > MAX_HEADER_LINE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP header line exceeds maximum length",
                ));
            }
            buf.extend_from_slice(&available[..take_len]);
            reader.consume(take_len);
            return Ok(total + take_len);
        }
        let chunk_len = available.len();
        if total + chunk_len > MAX_HEADER_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP header line exceeds maximum length",
            ));
        }
        buf.extend_from_slice(available);
        reader.consume(chunk_len);
        total += chunk_len;
    }
}

/// Read one `Content-Length`-framed LSP message from `reader`.
/// Returns `Ok(None)` cleanly on EOF before any header bytes.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut raw_line = Vec::new();
    let mut saw_any_header = false;
    let mut header_count = 0usize;

    loop {
        let n = read_bounded_line(reader, &mut raw_line)?;
        if n == 0 {
            if !saw_any_header {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "EOF while reading LSP headers",
            ));
        }
        let line = std::str::from_utf8(&raw_line).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("non-UTF-8 LSP header: {e}"),
            )
        })?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if saw_any_header {
                break;
            }
            continue;
        }
        header_count += 1;
        if header_count > MAX_HEADER_COUNT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many LSP header lines",
            ));
        }
        saw_any_header = true;
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("Content-Length")
        {
            let parsed = value.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length header")
            })?;
            if parsed > MAX_RPC_PAYLOAD_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "LSP payload Content-Length ({parsed}) exceeds maximum ({MAX_RPC_PAYLOAD_BYTES} bytes)"
                    ),
                ));
            }
            content_length = Some(parsed);
        }
    }

    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(len)
        .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "cannot allocate RPC payload"))?;
    let read_bytes = reader.take(len as u64).read_to_end(&mut buf)?;
    if read_bytes != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "EOF while reading LSP payload body",
        ));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Write a `Content-Length`-framed JSON-RPC payload to `writer` and flush.
pub fn write_message<W: Write>(writer: &mut W, payload: &JsonValue) -> io::Result<()> {
    let body = payload.to_json_string();
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()
}

pub fn read_rpc_message<R: BufRead>(reader: &mut R) -> io::Result<Option<RpcMessage>> {
    let Some(raw) = read_message(reader)? else {
        return Ok(None);
    };
    let val = JsonValue::parse(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let msg = RpcMessage::from_json(val)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.message))?;
    Ok(Some(msg))
}

pub fn send_response<W: Write>(writer: &mut W, id: RequestId, result: JsonValue) -> io::Result<()> {
    write_message(writer, &RpcMessage::ok_response(id, result))
}

pub fn send_error<W: Write>(
    writer: &mut W,
    id: RequestId,
    code: i64,
    message: &str,
) -> io::Result<()> {
    write_message(
        writer,
        &RpcMessage::error_response(id, RpcError::new(code, message)),
    )
}

pub fn send_notification<W: Write>(
    writer: &mut W,
    method: &str,
    params: JsonValue,
) -> io::Result<()> {
    write_message(writer, &RpcMessage::notification(method, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_and_rejects_oversized_content_length_without_oom() {
        let mut wire = Vec::new();
        send_notification(&mut wire, "initialized", JsonValue::empty_object()).unwrap();
        send_response(&mut wire, RequestId::Int(7), JsonValue::from("ok")).unwrap();

        let mut reader = Cursor::new(wire);
        let first = read_rpc_message(&mut reader).unwrap().unwrap();
        assert!(
            matches!(first, RpcMessage::Notification { method, .. } if method == "initialized")
        );
        let second = read_rpc_message(&mut reader).unwrap().unwrap();
        assert!(matches!(
            second,
            RpcMessage::Response {
                id: RequestId::Int(7),
                ..
            }
        ));
        assert!(read_rpc_message(&mut reader).unwrap().is_none());

        let huge = format!("Content-Length: {}\r\n\r\n{{}}", MAX_RPC_PAYLOAD_BYTES + 1);
        let mut bad_reader = Cursor::new(huge.into_bytes());
        let err = read_rpc_message(&mut bad_reader).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
