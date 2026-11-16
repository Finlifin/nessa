//! `std.net` non-blocking `libuv` TCP, UDP, and DNS builtins (`847..=864`).

use interpreter::{BuiltinCtx, NativeValue, VmError};

const IO_INVALID_INPUT: i64 = 4;

/// `847` `__net_tcp_listen(host: String, port: i64, backlog: i64) -> (i64, String, i64, String, i64)`
pub fn tcp_listen(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.run_net_op(|ctx| {
        let host = ctx.arg_string(0)?;
        let port = ctx.arg_i64(1)?;
        let backlog = ctx.arg_i64(2)?;
        if backlog <= 0 {
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(IO_INVALID_INPUT),
                NativeValue::Str("backlog must be >= 1".to_string()),
                NativeValue::I64(-1),
                NativeValue::Str(String::new()),
                NativeValue::I64(0),
            ]))?;
            return Ok(false);
        }
        ctx.submit_tcp_listen(host, port, backlog);
        Ok(true)
    })
}

/// `848` `__net_tcp_accept(listener_id: i64) -> (i64, String, i64, String, i64)`
pub fn tcp_accept(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        ctx.submit_tcp_accept(id);
        Ok(true)
    })
}

/// `849` `__net_tcp_connect(host: String, port: i64) -> (i64, String, i64, String, i64)`
pub fn tcp_connect(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_net_op(|ctx| {
        let host = ctx.arg_string(0)?;
        let port = ctx.arg_i64(1)?;
        ctx.submit_tcp_connect(host, port);
        Ok(true)
    })
}

/// `850` `__net_tcp_read(stream_id: i64, max_bytes: i64) -> (i64, String, List)`
pub fn tcp_read(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        let max_bytes = ctx.arg_i64(1)?;
        if max_bytes < 0 {
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(IO_INVALID_INPUT),
                NativeValue::Str("max_bytes must be >= 0".to_string()),
                NativeValue::List(Vec::new()),
            ]))?;
            return Ok(false);
        }
        let max_usize = usize::try_from(max_bytes)
            .map_err(|_| VmError::NumericOverflow)?
            .min(16 * 1024 * 1024);
        ctx.submit_tcp_read_bytes(id, max_usize);
        Ok(true)
    })
}

/// `851` `__net_tcp_read_line(stream_id: i64) -> (i64, String, ?String)`
pub fn tcp_read_line(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        ctx.submit_tcp_read_line(id);
        Ok(true)
    })
}

/// `852` `__net_tcp_write_str(stream_id: i64, data: String) -> (i64, String, i64)`
pub fn tcp_write_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        let data = ctx.arg_string(1)?;
        ctx.submit_tcp_write(id, data.into_bytes());
        Ok(true)
    })
}

/// `853` `__net_tcp_write_bytes(stream_id: i64, bytes: List) -> (i64, String, i64)`
pub fn tcp_write_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        let list_root = ctx.arg_rooted(1)?;
        match ctx.extract_byte_list(&list_root)? {
            Ok(bytes) => {
                ctx.submit_tcp_write(id, bytes);
                Ok(true)
            }
            Err(msg) => {
                ctx.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(IO_INVALID_INPUT),
                    NativeValue::Str(msg),
                    NativeValue::I64(0),
                ]))?;
                Ok(false)
            }
        }
    })
}

/// `854` `__net_tcp_set_nodelay(stream_id: i64, enable: bool) -> (i64, String, i64)`
pub fn tcp_set_nodelay(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        let enable = ctx.arg_bool(1)?;
        ctx.submit_tcp_set_nodelay(id, enable);
        Ok(true)
    })
}

/// `855` `__net_tcp_local_addr(id: i64) -> (i64, String, String, i64)`
pub fn tcp_local_addr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.net_tcp_local_addr(id)
}

/// `856` `__net_tcp_peer_addr(id: i64) -> (i64, String, String, i64)`
pub fn tcp_peer_addr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.net_tcp_peer_addr(id)
}

/// `857` `__net_tcp_close(id: i64) -> Unit`
pub fn tcp_close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.net_tcp_close(id)
}

/// `858` `__net_udp_bind(host: String, port: i64) -> (i64, String, i64, String, i64)`
pub fn udp_bind(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_net_op(|ctx| {
        let host = ctx.arg_string(0)?;
        let port = ctx.arg_i64(1)?;
        ctx.submit_udp_bind(host, port);
        Ok(true)
    })
}

/// `859` `__net_udp_send_str(sock_id: i64, host: String, port: i64, data: String) -> (i64, String, i64)`
pub fn udp_send_str(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(4)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        let host = ctx.arg_string(1)?;
        let port = ctx.arg_i64(2)?;
        let data = ctx.arg_string(3)?;
        ctx.submit_udp_send(id, host, port, data.into_bytes());
        Ok(true)
    })
}

/// `860` `__net_udp_send_bytes(sock_id: i64, host: String, port: i64, bytes: List) -> (i64, String, i64)`
pub fn udp_send_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(4)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        let host = ctx.arg_string(1)?;
        let port = ctx.arg_i64(2)?;
        let list_root = ctx.arg_rooted(3)?;
        match ctx.extract_byte_list(&list_root)? {
            Ok(bytes) => {
                ctx.submit_udp_send(id, host, port, bytes);
                Ok(true)
            }
            Err(msg) => {
                ctx.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(IO_INVALID_INPUT),
                    NativeValue::Str(msg),
                    NativeValue::I64(0),
                ]))?;
                Ok(false)
            }
        }
    })
}

/// `861` `__net_udp_recv(sock_id: i64, max_bytes: i64) -> (i64, String, List, String, i64)`
pub fn udp_recv(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.run_net_op(|ctx| {
        let id = ctx.arg_i64(0)?;
        let max_bytes = ctx.arg_i64(1)?;
        if max_bytes <= 0 {
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(IO_INVALID_INPUT),
                NativeValue::Str("max_bytes must be >= 1".to_string()),
                NativeValue::List(Vec::new()),
                NativeValue::Str(String::new()),
                NativeValue::I64(0),
            ]))?;
            return Ok(false);
        }
        let max_usize = usize::try_from(max_bytes)
            .map_err(|_| VmError::NumericOverflow)?
            .min(65536);
        ctx.submit_udp_recv(id, max_usize);
        Ok(true)
    })
}

/// `862` `__net_udp_local_addr(id: i64) -> (i64, String, String, i64)`
pub fn udp_local_addr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.net_udp_local_addr(id)
}

/// `863` `__net_udp_close(id: i64) -> Unit`
pub fn udp_close(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let id = ctx.arg_i64(0)?;
    ctx.net_udp_close(id)
}

/// `864` `__net_resolve_host(host: String) -> (i64, String, List)`
pub fn resolve_host(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.run_net_op(|ctx| {
        let host = ctx.arg_string(0)?;
        ctx.submit_resolve_host(host);
        Ok(true)
    })
}
