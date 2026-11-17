//! Non-blocking TCP, UDP, and DNS reactor operations on `libuv` (`std.net`).
//!
//! All `uv_tcp_t`, `uv_udp_t`, `uv_connect_t`, `uv_write_t`, `uv_udp_send_t`, and
//! `uv_getaddrinfo_t` handles/requests are owned and mutated exclusively on the single
//! `nessa-io-reactor` thread. Compute workers submit [`NetCmd`] messages over the reactor
//! doorbell and suspend via `TaskOpEntry` without blocking OS worker threads.
//!
//! Idle open sockets are `uv_unref`-ed whenever no in-flight read/accept/recv/connect/write
//! operation is active on them, and `active_io_handles` is incremented strictly per suspended
//! `TaskOpEntry`, preserving deterministic `VmError::DeadlockDetected` when tasks deadlock
//! while holding an idle socket.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use parking_lot::Mutex;
use runtime::FastHashMap;

use crate::reactor::TaskOpEntry;

pub(crate) const MAX_BUFFERED_TCP_READ_BYTES: usize = 64 * 1024;

/// Result payload returned to compute workers for `std.net` operations.
#[derive(Debug)]
pub enum NetOutcome {
    /// `(code, msg, handle_id, host, port)`
    HandleAddr {
        code: i64,
        msg: String,
        handle_id: i64,
        host: String,
        port: i64,
    },
    /// `(code, msg, bytes)`
    Bytes {
        code: i64,
        msg: String,
        data: Vec<u8>,
    },
    /// `(code, msg, line)` where `None` indicates clean EOF.
    Line {
        code: i64,
        msg: String,
        line: Option<String>,
    },
    /// `(code, msg, count)`
    Count { code: i64, msg: String, count: i64 },
    /// `(code, msg, bytes, host, port)`
    Datagram {
        code: i64,
        msg: String,
        data: Vec<u8>,
        host: String,
        port: i64,
    },
    /// `(code, msg, ip_addresses)`
    Hosts {
        code: i64,
        msg: String,
        addrs: Vec<String>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetHandleKind {
    TcpListener,
    TcpStream,
    UdpSocket,
}

pub(crate) struct NetHandleMeta {
    pub(crate) kind: NetHandleKind,
    pub(crate) local_addr: (String, i64),
    pub(crate) peer_addr: (String, i64),
    pub(crate) closed: AtomicBool,
}

/// Shared metadata registry allowing instant `local_addr()`, `peer_addr()`, and idempotent `close()`
/// validation from compute workers without a reactor round-trip.
pub(crate) struct NetRegistry {
    next_id: AtomicI64,
    handles: Mutex<FastHashMap<i64, Arc<NetHandleMeta>>>,
}

impl NetRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            handles: Mutex::new(FastHashMap::default()),
        }
    }

    pub(crate) fn alloc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn insert(&self, id: i64, meta: Arc<NetHandleMeta>) {
        self.handles.lock().insert(id, meta);
    }

    pub(crate) fn get_open(&self, id: i64, expected: NetHandleKind) -> Option<Arc<NetHandleMeta>> {
        let map = self.handles.lock();
        let meta = map.get(&id)?;
        if meta.kind != expected || meta.closed.load(Ordering::Acquire) {
            return None;
        }
        Some(Arc::clone(meta))
    }

    /// Atomically mark `id` closed if it matches `expected` and was still open.
    /// Returns `Ok(true)` if newly closed, `Ok(false)` if already closed, `Err(())` if wrong kind/unknown.
    pub(crate) fn mark_closed(&self, id: i64, expected: NetHandleKind) -> Result<bool, ()> {
        let map = self.handles.lock();
        let meta = map.get(&id).ok_or(())?;
        if meta.kind != expected {
            return Err(());
        }
        Ok(!meta.closed.swap(true, Ordering::AcqRel))
    }

    pub(crate) fn clear(&self) {
        self.handles.lock().clear();
    }
}

pub(crate) enum TcpReadMode {
    Bytes(usize),
    Line,
}

pub(crate) enum NetCmd {
    TcpListen {
        op_id: u64,
        host: String,
        port: i64,
        backlog: i64,
        entry: Arc<TaskOpEntry>,
    },
    TcpAccept {
        op_id: u64,
        listener_id: i64,
        entry: Arc<TaskOpEntry>,
    },
    TcpConnect {
        op_id: u64,
        host: String,
        port: i64,
        entry: Arc<TaskOpEntry>,
    },
    TcpRead {
        op_id: u64,
        stream_id: i64,
        mode: TcpReadMode,
        entry: Arc<TaskOpEntry>,
    },
    TcpWrite {
        op_id: u64,
        stream_id: i64,
        data: Vec<u8>,
        entry: Arc<TaskOpEntry>,
    },
    TcpSetNodelay {
        op_id: u64,
        stream_id: i64,
        enable: bool,
        entry: Arc<TaskOpEntry>,
    },
    TcpCloseListener {
        listener_id: i64,
    },
    TcpCloseStream {
        stream_id: i64,
    },
    UdpBind {
        op_id: u64,
        host: String,
        port: i64,
        entry: Arc<TaskOpEntry>,
    },
    UdpSend {
        op_id: u64,
        sock_id: i64,
        host: String,
        port: i64,
        data: Vec<u8>,
        entry: Arc<TaskOpEntry>,
    },
    UdpRecv {
        op_id: u64,
        sock_id: i64,
        max_bytes: usize,
        entry: Arc<TaskOpEntry>,
    },
    UdpClose {
        sock_id: i64,
    },
    ResolveHost {
        op_id: u64,
        host: String,
        entry: Arc<TaskOpEntry>,
    },
}

pub(crate) fn normalize_host(host: &str) -> &str {
    let trimmed = host.trim();
    if trimmed.eq_ignore_ascii_case("localhost") {
        "127.0.0.1"
    } else if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        inner
    } else {
        trimmed
    }
}

#[cfg(unix)]
pub(crate) use unix_net::ReactorNetState;

#[cfg(unix)]
mod unix_net {
    use std::collections::VecDeque;
    use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
    use std::ptr;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Weak};

    use runtime::FastHashMap;

    use super::{
        MAX_BUFFERED_TCP_READ_BYTES, NetCmd, NetHandleKind, NetHandleMeta, NetOutcome, NetRegistry,
        TcpReadMode, normalize_host,
    };
    use crate::SchedulerInner;
    use crate::reactor::{
        TaskOpEntry, TaskOpResult, TaskOpState, alloc_uv_aligned, complete_entry, free_uv_aligned,
        try_complete_entry, uv_error_message, uv_error_to_io_code,
    };

    const IO_OK: i64 = 0;
    const IO_INVALID_INPUT: i64 = 4;
    const IO_CLOSED: i64 = 6;
    const IO_OTHER: i64 = 8;

    #[repr(C, align(8))]
    #[derive(Clone, Copy)]
    struct SockAddrBuf([u64; 16]);

    impl SockAddrBuf {
        fn zeroed() -> Self {
            Self([0u64; 16])
        }

        fn as_sockaddr_ptr(&self) -> *const libuv_sys2::sockaddr {
            self.0.as_ptr() as *const libuv_sys2::sockaddr
        }

        fn as_sockaddr_mut_ptr(&mut self) -> *mut libuv_sys2::sockaddr {
            self.0.as_mut_ptr() as *mut libuv_sys2::sockaddr
        }
    }

    fn parse_ip_port(host: &str, port: i64) -> Result<SockAddrBuf, (i64, String)> {
        if !(0..=65535).contains(&port) {
            return Err((
                IO_INVALID_INPUT,
                format!("port {port} out of valid range 0..=65535"),
            ));
        }
        let norm = normalize_host(host);
        if norm.is_empty() {
            return Err((IO_INVALID_INPUT, "host must not be empty".to_string()));
        }
        let c_host = CString::new(norm).map_err(|_| {
            (
                IO_INVALID_INPUT,
                "host contains interior NUL byte".to_string(),
            )
        })?;
        let mut storage = SockAddrBuf::zeroed();
        // SAFETY: `storage` is 128 bytes aligned to 8 bytes, exceeding `sizeof(sockaddr_in)` (16)
        // and `sizeof(sockaddr_in6)` (28).
        unsafe {
            let rc4 = libuv_sys2::uv_ip4_addr(
                c_host.as_ptr(),
                port as c_int,
                storage.0.as_mut_ptr() as *mut libuv_sys2::sockaddr_in,
            );
            if rc4 == 0 {
                return Ok(storage);
            }
            storage = SockAddrBuf::zeroed();
            let rc6 = libuv_sys2::uv_ip6_addr(
                c_host.as_ptr(),
                port as c_int,
                storage.0.as_mut_ptr() as *mut libuv_sys2::sockaddr_in6,
            );
            if rc6 == 0 {
                return Ok(storage);
            }
        }
        Err((
            IO_INVALID_INPUT,
            format!("invalid IPv4/IPv6 address `{host}`"),
        ))
    }

    unsafe fn extract_sockaddr(addr: *const libuv_sys2::sockaddr) -> Option<(String, i64)> {
        if addr.is_null() {
            return None;
        }
        // SAFETY: Caller passes a valid `sockaddr` (`sockaddr_in` or `sockaddr_in6`).
        unsafe {
            let family = (*addr).sa_family as u32;
            let mut ip_buf = [0 as c_char; 64];
            if family == libuv_sys2::AF_INET {
                let sin = &*(addr as *const libuv_sys2::sockaddr_in);
                if libuv_sys2::uv_ip4_name(sin, ip_buf.as_mut_ptr(), ip_buf.len()) != 0 {
                    return None;
                }
                let ip = CStr::from_ptr(ip_buf.as_ptr())
                    .to_string_lossy()
                    .into_owned();
                let port = u16::from_be(sin.sin_port) as i64;
                Some((ip, port))
            } else if family == libuv_sys2::AF_INET6 {
                let sin6 = &*(addr as *const libuv_sys2::sockaddr_in6);
                if libuv_sys2::uv_ip6_name(sin6, ip_buf.as_mut_ptr(), ip_buf.len()) != 0 {
                    return None;
                }
                let ip = CStr::from_ptr(ip_buf.as_ptr())
                    .to_string_lossy()
                    .into_owned();
                let port = u16::from_be(sin6.sin6_port) as i64;
                Some((ip, port))
            } else {
                None
            }
        }
    }

    unsafe fn tcp_sockname(
        handle: *const libuv_sys2::uv_tcp_t,
    ) -> Result<(String, i64), (i64, String)> {
        let mut storage = SockAddrBuf::zeroed();
        let mut namelen = std::mem::size_of::<SockAddrBuf>() as c_int;
        // SAFETY: `handle` is an initialized `uv_tcp_t` and `storage` is 128 bytes.
        let rc = unsafe {
            libuv_sys2::uv_tcp_getsockname(handle, storage.as_sockaddr_mut_ptr(), &mut namelen)
        };
        if rc < 0 {
            return Err((uv_error_to_io_code(rc), uv_error_message(rc)));
        }
        // SAFETY: `uv_tcp_getsockname` populated a valid `sockaddr` into `storage`.
        unsafe { extract_sockaddr(storage.as_sockaddr_ptr()) }
            .ok_or_else(|| (IO_OTHER, "unsupported socket address family".to_string()))
    }

    unsafe fn tcp_peername(
        handle: *const libuv_sys2::uv_tcp_t,
    ) -> Result<(String, i64), (i64, String)> {
        let mut storage = SockAddrBuf::zeroed();
        let mut namelen = std::mem::size_of::<SockAddrBuf>() as c_int;
        // SAFETY: `handle` is an initialized connected `uv_tcp_t` and `storage` is 128 bytes.
        let rc = unsafe {
            libuv_sys2::uv_tcp_getpeername(handle, storage.as_sockaddr_mut_ptr(), &mut namelen)
        };
        if rc < 0 {
            return Err((uv_error_to_io_code(rc), uv_error_message(rc)));
        }
        // SAFETY: `uv_tcp_getpeername` populated a valid `sockaddr` into `storage`.
        unsafe { extract_sockaddr(storage.as_sockaddr_ptr()) }
            .ok_or_else(|| (IO_OTHER, "unsupported peer address family".to_string()))
    }

    unsafe fn udp_sockname(
        handle: *const libuv_sys2::uv_udp_t,
    ) -> Result<(String, i64), (i64, String)> {
        let mut storage = SockAddrBuf::zeroed();
        let mut namelen = std::mem::size_of::<SockAddrBuf>() as c_int;
        // SAFETY: `handle` is an initialized bound `uv_udp_t` and `storage` is 128 bytes.
        let rc = unsafe {
            libuv_sys2::uv_udp_getsockname(handle, storage.as_sockaddr_mut_ptr(), &mut namelen)
        };
        if rc < 0 {
            return Err((uv_error_to_io_code(rc), uv_error_message(rc)));
        }
        // SAFETY: `uv_udp_getsockname` populated a valid `sockaddr` into `storage`.
        unsafe { extract_sockaddr(storage.as_sockaddr_ptr()) }
            .ok_or_else(|| (IO_OTHER, "unsupported UDP address family".to_string()))
    }

    // ---------------------------------------------------------------------------
    // Reactor-thread network state & per-handle contexts
    // ---------------------------------------------------------------------------

    enum NetCancelTarget {
        TcpAccept(i64),
        TcpConnect(*mut TcpConnectCtx),
        TcpRead(i64),
        TcpWrite,
        UdpRecv(i64),
        UdpSend,
        Dns(*mut libuv_sys2::uv_getaddrinfo_t),
    }

    pub(crate) struct ReactorNetState {
        loop_ptr: *mut libuv_sys2::uv_loop_t,
        sched: Weak<SchedulerInner>,
        registry: Arc<NetRegistry>,
        listeners: FastHashMap<i64, *mut TcpListenerCtx>,
        streams: FastHashMap<i64, *mut TcpStreamCtx>,
        udps: FastHashMap<i64, *mut UdpSocketCtx>,
        cancel_targets: FastHashMap<u64, NetCancelTarget>,
        shutting_down: bool,
    }

    struct TcpListenerCtx {
        handle: *mut libuv_sys2::uv_tcp_t,
        net: *mut ReactorNetState,
        closing: bool,
        backlog: VecDeque<(i64, String, i64)>,
        accept_waiters: VecDeque<(u64, Arc<TaskOpEntry>)>,
    }

    struct PendingTcpRead {
        op_id: u64,
        mode: TcpReadMode,
        entry: Arc<TaskOpEntry>,
    }

    struct TcpStreamCtx {
        handle: *mut libuv_sys2::uv_tcp_t,
        net: *mut ReactorNetState,
        closing: bool,
        reading: bool,
        eof: bool,
        read_err: Option<(i64, String)>,
        read_buf: VecDeque<u8>,
        pending_read: Option<PendingTcpRead>,
    }

    struct TcpConnectCtx {
        op_id: u64,
        tcp_handle: *mut libuv_sys2::uv_tcp_t,
        connect_req: *mut libuv_sys2::uv_connect_t,
        dns_req: *mut libuv_sys2::uv_getaddrinfo_t,
        port: i64,
        net: *mut ReactorNetState,
        entry: Arc<TaskOpEntry>,
        cancelled: bool,
        tcp_closing: bool,
        req_done: bool,
    }

    struct TcpWriteCtx {
        op_id: u64,
        data: Vec<u8>,
        net: *mut ReactorNetState,
        entry: Arc<TaskOpEntry>,
    }

    struct UdpPacket {
        data: Vec<u8>,
        host: String,
        port: i64,
    }

    struct UdpSocketCtx {
        handle: *mut libuv_sys2::uv_udp_t,
        net: *mut ReactorNetState,
        closing: bool,
        receiving: bool,
        recv_err: Option<(i64, String)>,
        recv_queue: VecDeque<UdpPacket>,
        recv_waiters: VecDeque<(u64, usize, Arc<TaskOpEntry>)>,
    }

    struct UdpSendCtx {
        op_id: u64,
        data: Vec<u8>,
        net: *mut ReactorNetState,
        entry: Arc<TaskOpEntry>,
    }

    struct DnsCtx {
        op_id: u64,
        net: *mut ReactorNetState,
        entry: Arc<TaskOpEntry>,
    }

    type StreamConnMeta = (i64, (String, i64), (String, i64));
    type IoErrTuple = (i64, String);

    impl ReactorNetState {
        pub(crate) fn new(
            loop_ptr: *mut libuv_sys2::uv_loop_t,
            sched: Weak<SchedulerInner>,
            registry: Arc<NetRegistry>,
        ) -> Box<Self> {
            Box::new(Self {
                loop_ptr,
                sched,
                registry,
                listeners: FastHashMap::default(),
                streams: FastHashMap::default(),
                udps: FastHashMap::default(),
                cancel_targets: FastHashMap::default(),
                shutting_down: false,
            })
        }

        pub(crate) fn handle_cmd(&mut self, cmd: NetCmd) {
            if self.shutting_down {
                return;
            }
            match cmd {
                NetCmd::TcpListen {
                    op_id,
                    host,
                    port,
                    backlog,
                    entry,
                } => self.cmd_tcp_listen(op_id, &host, port, backlog, entry),
                NetCmd::TcpAccept {
                    op_id,
                    listener_id,
                    entry,
                } => self.cmd_tcp_accept(op_id, listener_id, entry),
                NetCmd::TcpConnect {
                    op_id,
                    host,
                    port,
                    entry,
                } => self.cmd_tcp_connect(op_id, &host, port, entry),
                NetCmd::TcpRead {
                    op_id,
                    stream_id,
                    mode,
                    entry,
                } => self.cmd_tcp_read(op_id, stream_id, mode, entry),
                NetCmd::TcpWrite {
                    op_id,
                    stream_id,
                    data,
                    entry,
                } => self.cmd_tcp_write(op_id, stream_id, data, entry),
                NetCmd::TcpSetNodelay {
                    op_id,
                    stream_id,
                    enable,
                    entry,
                } => self.cmd_tcp_set_nodelay(op_id, stream_id, enable, entry),
                NetCmd::TcpCloseListener { listener_id } => self.close_listener_by_id(listener_id),
                NetCmd::TcpCloseStream { stream_id } => self.close_stream_by_id(stream_id),
                NetCmd::UdpBind {
                    op_id,
                    host,
                    port,
                    entry,
                } => self.cmd_udp_bind(op_id, &host, port, entry),
                NetCmd::UdpSend {
                    op_id,
                    sock_id,
                    host,
                    port,
                    data,
                    entry,
                } => self.cmd_udp_send(op_id, sock_id, &host, port, data, entry),
                NetCmd::UdpRecv {
                    op_id,
                    sock_id,
                    max_bytes,
                    entry,
                } => self.cmd_udp_recv(op_id, sock_id, max_bytes, entry),
                NetCmd::UdpClose { sock_id } => self.close_udp_by_id(sock_id),
                NetCmd::ResolveHost { op_id, host, entry } => {
                    self.cmd_resolve_host(op_id, &host, entry)
                }
            }
        }

        pub(crate) fn cancel_op(&mut self, op_id: u64) -> bool {
            let Some(target) = self.cancel_targets.remove(&op_id) else {
                return false;
            };
            match target {
                NetCancelTarget::TcpAccept(listener_id) => {
                    if let Some(&lptr) = self.listeners.get(&listener_id) {
                        // SAFETY: `lptr` is owned on the reactor thread.
                        let listener = unsafe { &mut *lptr };
                        listener.accept_waiters.retain(|(oid, _)| *oid != op_id);
                    }
                }
                NetCancelTarget::TcpConnect(cptr) => {
                    // SAFETY: `cptr` is owned on the reactor thread while registered in `cancel_targets`.
                    unsafe { cancel_tcp_connect_ctx(cptr) };
                }
                NetCancelTarget::TcpRead(stream_id) => {
                    if let Some(&sptr) = self.streams.get(&stream_id) {
                        // SAFETY: `sptr` is owned on the reactor thread.
                        let stream = unsafe { &mut *sptr };
                        if stream
                            .pending_read
                            .as_ref()
                            .is_some_and(|pr| pr.op_id == op_id)
                        {
                            stream.pending_read = None;
                            stop_stream_reading(stream);
                        }
                    }
                }
                NetCancelTarget::TcpWrite | NetCancelTarget::UdpSend => {
                    // In-flight kernel write/send completes quickly and `complete_entry` ignores
                    // cancelled `TaskOpEntry`s while freeing `TcpWriteCtx` / `UdpSendCtx`.
                }
                NetCancelTarget::UdpRecv(sock_id) => {
                    if let Some(&uptr) = self.udps.get(&sock_id) {
                        // SAFETY: `uptr` is owned on the reactor thread.
                        let sock = unsafe { &mut *uptr };
                        sock.recv_waiters.retain(|(oid, _, _)| *oid != op_id);
                        if sock.recv_waiters.is_empty() {
                            stop_udp_receiving(sock);
                        }
                    }
                }
                NetCancelTarget::Dns(req_ptr) => {
                    // SAFETY: `uv_cancel` cancels queued `uv_getaddrinfo_t` requests; `on_getaddrinfo`
                    // still runs and frees `req_ptr` + `DnsCtx`.
                    unsafe {
                        let _ = libuv_sys2::uv_cancel(req_ptr as *mut libuv_sys2::uv_req_t);
                    }
                }
            }
            true
        }

        pub(crate) fn begin_shutdown(&mut self) {
            if self.shutting_down {
                return;
            }
            self.shutting_down = true;
            let cancel_keys: Vec<u64> = self.cancel_targets.keys().copied().collect();
            for op_id in cancel_keys {
                let _ = self.cancel_op(op_id);
            }
            let listener_ids: Vec<i64> = self.listeners.keys().copied().collect();
            for id in listener_ids {
                self.close_listener_by_id(id);
            }
            let stream_ids: Vec<i64> = self.streams.keys().copied().collect();
            for id in stream_ids {
                self.close_stream_by_id(id);
            }
            let udp_ids: Vec<i64> = self.udps.keys().copied().collect();
            for id in udp_ids {
                self.close_udp_by_id(id);
            }
        }

        // -- TCP Listener ---------------------------------------------------------

        fn cmd_tcp_listen(
            &mut self,
            _op_id: u64,
            host: &str,
            port: i64,
            backlog: i64,
            entry: Arc<TaskOpEntry>,
        ) {
            let addr = match parse_ip_port(host, port) {
                Ok(a) => a,
                Err((code, msg)) => {
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code,
                            msg,
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
            };
            let backlog_c = backlog.clamp(1, 4096) as c_int;
            let net_ptr = self as *mut Self;
            // SAFETY: Initialize, bind, and listen on a fresh `uv_tcp_t` on `self.loop_ptr`.
            unsafe {
                let tcp_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_TCP);
                let handle = alloc_uv_aligned::<libuv_sys2::uv_tcp_t>(tcp_size);
                let rc_init = libuv_sys2::uv_tcp_init(self.loop_ptr, handle);
                if rc_init < 0 {
                    free_uv_aligned(handle);
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc_init),
                            msg: uv_error_message(rc_init),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                let rc_bind = libuv_sys2::uv_tcp_bind(handle, addr.as_sockaddr_ptr(), 0);
                if rc_bind < 0 {
                    libuv_sys2::uv_close(
                        handle as *mut libuv_sys2::uv_handle_t,
                        Some(on_bare_uv_close),
                    );
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc_bind),
                            msg: uv_error_message(rc_bind),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                let id = self.registry.alloc_id();
                let ctx_ptr = Box::into_raw(Box::new(TcpListenerCtx {
                    handle,
                    net: net_ptr,
                    closing: false,
                    backlog: VecDeque::new(),
                    accept_waiters: VecDeque::new(),
                }));
                libuv_sys2::uv_handle_set_data(
                    handle as *mut libuv_sys2::uv_handle_t,
                    ctx_ptr as *mut c_void,
                );
                let rc_listen = libuv_sys2::uv_listen(
                    handle as *mut libuv_sys2::uv_stream_t,
                    backlog_c,
                    Some(on_tcp_connection),
                );
                if rc_listen < 0 {
                    (*ctx_ptr).closing = true;
                    libuv_sys2::uv_close(
                        handle as *mut libuv_sys2::uv_handle_t,
                        Some(on_listener_close),
                    );
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc_listen),
                            msg: uv_error_message(rc_listen),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                let (local_ip, local_port) = match tcp_sockname(handle) {
                    Ok(pair) => pair,
                    Err((code, msg)) => {
                        (*ctx_ptr).closing = true;
                        libuv_sys2::uv_close(
                            handle as *mut libuv_sys2::uv_handle_t,
                            Some(on_listener_close),
                        );
                        complete_entry(
                            &entry,
                            TaskOpResult::Net(NetOutcome::HandleAddr {
                                code,
                                msg,
                                handle_id: -1,
                                host: String::new(),
                                port: 0,
                            }),
                            &self.sched,
                        );
                        return;
                    }
                };
                self.registry.insert(
                    id,
                    Arc::new(NetHandleMeta {
                        kind: NetHandleKind::TcpListener,
                        local_addr: (local_ip.clone(), local_port),
                        peer_addr: (String::new(), 0),
                        closed: AtomicBool::new(false),
                    }),
                );
                self.listeners.insert(id, ctx_ptr);
                if try_complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_OK,
                        msg: String::new(),
                        handle_id: id,
                        host: local_ip,
                        port: local_port,
                    }),
                    &self.sched,
                )
                .is_err()
                {
                    self.close_listener_by_id(id);
                }
            }
        }

        fn cmd_tcp_accept(&mut self, op_id: u64, listener_id: i64, entry: Arc<TaskOpEntry>) {
            let Some(&lptr) = self.listeners.get(&listener_id) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_CLOSED,
                        msg: "TCP listener is closed".to_string(),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            };
            // SAFETY: `lptr` is owned on the reactor thread.
            let listener = unsafe { &mut *lptr };
            if listener.closing {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_CLOSED,
                        msg: "TCP listener is closed".to_string(),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
                return;
            }
            if let Some((stream_id, peer_ip, peer_port)) = listener.backlog.pop_front() {
                if let Err(TaskOpResult::Net(NetOutcome::HandleAddr {
                    handle_id,
                    host,
                    port,
                    ..
                })) = try_complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_OK,
                        msg: String::new(),
                        handle_id: stream_id,
                        host: peer_ip,
                        port: peer_port,
                    }),
                    &self.sched,
                ) {
                    listener.backlog.push_front((handle_id, host, port));
                }
                return;
            }
            listener.accept_waiters.push_back((op_id, entry));
            self.cancel_targets
                .insert(op_id, NetCancelTarget::TcpAccept(listener_id));
        }

        fn close_listener_by_id(&mut self, listener_id: i64) {
            let Some(lptr) = self.listeners.remove(&listener_id) else {
                return;
            };
            let _ = self
                .registry
                .mark_closed(listener_id, NetHandleKind::TcpListener);
            // SAFETY: `lptr` was removed from `self.listeners` and is owned on the reactor thread.
            unsafe {
                let listener = &mut *lptr;
                listener.closing = true;
                for (op_id, waiter) in listener.accept_waiters.drain(..) {
                    self.cancel_targets.remove(&op_id);
                    complete_entry(
                        &waiter,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: IO_CLOSED,
                            msg: "TCP listener closed".to_string(),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                }
                for (queued_stream_id, _, _) in listener.backlog.drain(..) {
                    self.close_stream_by_id(queued_stream_id);
                }
                libuv_sys2::uv_close(
                    listener.handle as *mut libuv_sys2::uv_handle_t,
                    Some(on_listener_close),
                );
            }
        }

        // -- TCP Connect & Stream -------------------------------------------------

        fn cmd_tcp_connect(&mut self, op_id: u64, host: &str, port: i64, entry: Arc<TaskOpEntry>) {
            if !(0..=65535).contains(&port) {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_INVALID_INPUT,
                        msg: format!("port {port} out of valid range 0..=65535"),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            if let Ok(addr) = parse_ip_port(host, port) {
                self.start_tcp_connect_sockaddr(op_id, &addr, port, entry);
                return;
            }
            // Hostname fallback via non-blocking `uv_getaddrinfo`.
            let norm = normalize_host(host);
            let Ok(c_host) = CString::new(norm) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_INVALID_INPUT,
                        msg: "host contains interior NUL byte".to_string(),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            };
            let net_ptr = self as *mut Self;
            // SAFETY: Allocate `uv_getaddrinfo_t` via `alloc_uv_aligned` and start async resolution.
            unsafe {
                let req_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_GETADDRINFO);
                let dns_req = alloc_uv_aligned::<libuv_sys2::uv_getaddrinfo_t>(req_size);
                let ctx_ptr = Box::into_raw(Box::new(TcpConnectCtx {
                    op_id,
                    tcp_handle: ptr::null_mut(),
                    connect_req: ptr::null_mut(),
                    dns_req,
                    port,
                    net: net_ptr,
                    entry: Arc::clone(&entry),
                    cancelled: false,
                    tcp_closing: false,
                    req_done: false,
                }));
                libuv_sys2::uv_req_set_data(
                    dns_req as *mut libuv_sys2::uv_req_t,
                    ctx_ptr as *mut c_void,
                );
                let rc = libuv_sys2::uv_getaddrinfo(
                    self.loop_ptr,
                    dns_req,
                    Some(on_connect_dns_resolved),
                    c_host.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                );
                if rc < 0 {
                    free_uv_aligned(dns_req);
                    drop(Box::from_raw(ctx_ptr));
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc),
                            msg: uv_error_message(rc),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                self.cancel_targets
                    .insert(op_id, NetCancelTarget::TcpConnect(ctx_ptr));
            }
        }

        fn start_tcp_connect_sockaddr(
            &mut self,
            op_id: u64,
            addr: &SockAddrBuf,
            port: i64,
            entry: Arc<TaskOpEntry>,
        ) {
            let net_ptr = self as *mut Self;
            // SAFETY: Allocate `uv_tcp_t` + `uv_connect_t` and initiate `uv_tcp_connect`.
            unsafe {
                let tcp_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_TCP);
                let tcp_handle = alloc_uv_aligned::<libuv_sys2::uv_tcp_t>(tcp_size);
                let rc_init = libuv_sys2::uv_tcp_init(self.loop_ptr, tcp_handle);
                if rc_init < 0 {
                    free_uv_aligned(tcp_handle);
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc_init),
                            msg: uv_error_message(rc_init),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                let req_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_CONNECT);
                let connect_req = alloc_uv_aligned::<libuv_sys2::uv_connect_t>(req_size);
                let ctx_ptr = Box::into_raw(Box::new(TcpConnectCtx {
                    op_id,
                    tcp_handle,
                    connect_req,
                    dns_req: ptr::null_mut(),
                    port,
                    net: net_ptr,
                    entry: Arc::clone(&entry),
                    cancelled: false,
                    tcp_closing: false,
                    req_done: false,
                }));
                libuv_sys2::uv_handle_set_data(
                    tcp_handle as *mut libuv_sys2::uv_handle_t,
                    ctx_ptr as *mut c_void,
                );
                libuv_sys2::uv_req_set_data(
                    connect_req as *mut libuv_sys2::uv_req_t,
                    ctx_ptr as *mut c_void,
                );
                let rc = libuv_sys2::uv_tcp_connect(
                    connect_req,
                    tcp_handle,
                    addr.as_sockaddr_ptr(),
                    Some(on_tcp_connect),
                );
                if rc < 0 {
                    free_uv_aligned(connect_req);
                    (*ctx_ptr).connect_req = ptr::null_mut();
                    (*ctx_ptr).req_done = true;
                    (*ctx_ptr).tcp_closing = true;
                    libuv_sys2::uv_close(
                        tcp_handle as *mut libuv_sys2::uv_handle_t,
                        Some(on_connect_tcp_close),
                    );
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc),
                            msg: uv_error_message(rc),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                self.cancel_targets
                    .insert(op_id, NetCancelTarget::TcpConnect(ctx_ptr));
            }
        }

        fn register_connected_stream(
            &mut self,
            handle: *mut libuv_sys2::uv_tcp_t,
        ) -> Result<StreamConnMeta, IoErrTuple> {
            // SAFETY: `handle` is an initialized connected `uv_tcp_t` on `self.loop_ptr`.
            let (local, peer) = unsafe {
                let local = tcp_sockname(handle)?;
                let peer = tcp_peername(handle)?;
                (local, peer)
            };
            let id = self.registry.alloc_id();
            let net_ptr = self as *mut Self;
            let sctx_ptr = Box::into_raw(Box::new(TcpStreamCtx {
                handle,
                net: net_ptr,
                closing: false,
                reading: false,
                eof: false,
                read_err: None,
                read_buf: VecDeque::new(),
                pending_read: None,
            }));
            // SAFETY: Rebind `handle` user data to `*mut TcpStreamCtx`.
            unsafe {
                libuv_sys2::uv_handle_set_data(
                    handle as *mut libuv_sys2::uv_handle_t,
                    sctx_ptr as *mut c_void,
                );
            }
            self.registry.insert(
                id,
                Arc::new(NetHandleMeta {
                    kind: NetHandleKind::TcpStream,
                    local_addr: local.clone(),
                    peer_addr: peer.clone(),
                    closed: AtomicBool::new(false),
                }),
            );
            self.streams.insert(id, sctx_ptr);
            Ok((id, local, peer))
        }

        fn cmd_tcp_read(
            &mut self,
            op_id: u64,
            stream_id: i64,
            mode: TcpReadMode,
            entry: Arc<TaskOpEntry>,
        ) {
            let Some(&sptr) = self.streams.get(&stream_id) else {
                complete_read_closed(&entry, &mode, &self.sched);
                return;
            };
            // SAFETY: `sptr` is owned on the reactor thread.
            let stream = unsafe { &mut *sptr };
            if stream.closing {
                complete_read_closed(&entry, &mode, &self.sched);
                return;
            }
            if stream.pending_read.is_some() {
                let out = match mode {
                    TcpReadMode::Bytes(_) => NetOutcome::Bytes {
                        code: IO_INVALID_INPUT,
                        msg: "concurrent read already in progress on TcpStream".to_string(),
                        data: Vec::new(),
                    },
                    TcpReadMode::Line => NetOutcome::Line {
                        code: IO_INVALID_INPUT,
                        msg: "concurrent read already in progress on TcpStream".to_string(),
                        line: None,
                    },
                };
                complete_entry(&entry, TaskOpResult::Net(out), &self.sched);
                return;
            }
            stream.pending_read = Some(PendingTcpRead { op_id, mode, entry });
            if try_satisfy_tcp_read(stream, &self.sched) {
                return;
            }
            if !stream.reading {
                // SAFETY: Start async reading on `stream.handle`.
                let rc = unsafe {
                    libuv_sys2::uv_read_start(
                        stream.handle as *mut libuv_sys2::uv_stream_t,
                        Some(on_net_alloc),
                        Some(on_tcp_read),
                    )
                };
                if rc < 0 {
                    stream.read_err = Some((uv_error_to_io_code(rc), uv_error_message(rc)));
                    let _ = try_satisfy_tcp_read(stream, &self.sched);
                    return;
                }
                stream.reading = true;
            }
            self.cancel_targets
                .insert(op_id, NetCancelTarget::TcpRead(stream_id));
        }

        fn cmd_tcp_write(
            &mut self,
            op_id: u64,
            stream_id: i64,
            data: Vec<u8>,
            entry: Arc<TaskOpEntry>,
        ) {
            let Some(&sptr) = self.streams.get(&stream_id) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_CLOSED,
                        msg: "TCP stream is closed".to_string(),
                        count: 0,
                    }),
                    &self.sched,
                );
                return;
            };
            // SAFETY: `sptr` is owned on the reactor thread.
            let stream = unsafe { &mut *sptr };
            if stream.closing {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_CLOSED,
                        msg: "TCP stream is closed".to_string(),
                        count: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            if data.is_empty() {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_OK,
                        msg: String::new(),
                        count: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            let net_ptr = self as *mut Self;
            // SAFETY: Allocate `uv_write_t` and keep `data` pinned inside `Box<TcpWriteCtx>` until `on_tcp_write`.
            unsafe {
                let req_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_WRITE);
                let req = alloc_uv_aligned::<libuv_sys2::uv_write_t>(req_size);
                let mut wctx = Box::new(TcpWriteCtx {
                    op_id,
                    data,
                    net: net_ptr,
                    entry: Arc::clone(&entry),
                });
                let buf = libuv_sys2::uv_buf_init(
                    wctx.data.as_mut_ptr() as *mut c_char,
                    wctx.data.len() as c_uint,
                );
                let wctx_ptr = Box::into_raw(wctx);
                libuv_sys2::uv_req_set_data(
                    req as *mut libuv_sys2::uv_req_t,
                    wctx_ptr as *mut c_void,
                );
                let rc = libuv_sys2::uv_write(
                    req,
                    stream.handle as *mut libuv_sys2::uv_stream_t,
                    &buf,
                    1,
                    Some(on_tcp_write),
                );
                if rc < 0 {
                    free_uv_aligned(req);
                    drop(Box::from_raw(wctx_ptr));
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::Count {
                            code: uv_error_to_io_code(rc),
                            msg: uv_error_message(rc),
                            count: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                self.cancel_targets.insert(op_id, NetCancelTarget::TcpWrite);
            }
        }

        fn cmd_tcp_set_nodelay(
            &mut self,
            _op_id: u64,
            stream_id: i64,
            enable: bool,
            entry: Arc<TaskOpEntry>,
        ) {
            let Some(&sptr) = self.streams.get(&stream_id) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_CLOSED,
                        msg: "TCP stream is closed".to_string(),
                        count: 0,
                    }),
                    &self.sched,
                );
                return;
            };
            // SAFETY: `sptr` is owned on the reactor thread.
            let stream = unsafe { &mut *sptr };
            if stream.closing {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_CLOSED,
                        msg: "TCP stream is closed".to_string(),
                        count: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            // SAFETY: `stream.handle` is an open `uv_tcp_t`.
            let rc = unsafe { libuv_sys2::uv_tcp_nodelay(stream.handle, c_int::from(enable)) };
            let (code, msg) = if rc < 0 {
                (uv_error_to_io_code(rc), uv_error_message(rc))
            } else {
                (IO_OK, String::new())
            };
            complete_entry(
                &entry,
                TaskOpResult::Net(NetOutcome::Count {
                    code,
                    msg,
                    count: 0,
                }),
                &self.sched,
            );
        }

        fn close_stream_by_id(&mut self, stream_id: i64) {
            let Some(sptr) = self.streams.remove(&stream_id) else {
                return;
            };
            let _ = self
                .registry
                .mark_closed(stream_id, NetHandleKind::TcpStream);
            // SAFETY: `sptr` was removed from `self.streams` and is owned on the reactor thread.
            unsafe {
                let stream = &mut *sptr;
                stream.closing = true;
                stop_stream_reading(stream);
                if let Some(pr) = stream.pending_read.take() {
                    self.cancel_targets.remove(&pr.op_id);
                    complete_read_closed(&pr.entry, &pr.mode, &self.sched);
                }
                libuv_sys2::uv_close(
                    stream.handle as *mut libuv_sys2::uv_handle_t,
                    Some(on_stream_close),
                );
            }
        }

        // -- UDP Socket -----------------------------------------------------------

        fn cmd_udp_bind(&mut self, _op_id: u64, host: &str, port: i64, entry: Arc<TaskOpEntry>) {
            let addr = match parse_ip_port(host, port) {
                Ok(a) => a,
                Err((code, msg)) => {
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code,
                            msg,
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
            };
            let net_ptr = self as *mut Self;
            // SAFETY: Allocate, init, and bind a fresh `uv_udp_t` on `self.loop_ptr`.
            unsafe {
                let udp_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_UDP);
                let handle = alloc_uv_aligned::<libuv_sys2::uv_udp_t>(udp_size);
                let rc_init = libuv_sys2::uv_udp_init(self.loop_ptr, handle);
                if rc_init < 0 {
                    free_uv_aligned(handle);
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc_init),
                            msg: uv_error_message(rc_init),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                let rc_bind = libuv_sys2::uv_udp_bind(handle, addr.as_sockaddr_ptr(), 0);
                if rc_bind < 0 {
                    libuv_sys2::uv_close(
                        handle as *mut libuv_sys2::uv_handle_t,
                        Some(on_bare_uv_close),
                    );
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc_bind),
                            msg: uv_error_message(rc_bind),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                let (local_ip, local_port) = match udp_sockname(handle) {
                    Ok(pair) => pair,
                    Err((code, msg)) => {
                        libuv_sys2::uv_close(
                            handle as *mut libuv_sys2::uv_handle_t,
                            Some(on_bare_uv_close),
                        );
                        complete_entry(
                            &entry,
                            TaskOpResult::Net(NetOutcome::HandleAddr {
                                code,
                                msg,
                                handle_id: -1,
                                host: String::new(),
                                port: 0,
                            }),
                            &self.sched,
                        );
                        return;
                    }
                };
                let id = self.registry.alloc_id();
                let uctx_ptr = Box::into_raw(Box::new(UdpSocketCtx {
                    handle,
                    net: net_ptr,
                    closing: false,
                    receiving: false,
                    recv_err: None,
                    recv_queue: VecDeque::new(),
                    recv_waiters: VecDeque::new(),
                }));
                libuv_sys2::uv_handle_set_data(
                    handle as *mut libuv_sys2::uv_handle_t,
                    uctx_ptr as *mut c_void,
                );
                self.registry.insert(
                    id,
                    Arc::new(NetHandleMeta {
                        kind: NetHandleKind::UdpSocket,
                        local_addr: (local_ip.clone(), local_port),
                        peer_addr: (String::new(), 0),
                        closed: AtomicBool::new(false),
                    }),
                );
                self.udps.insert(id, uctx_ptr);
                if try_complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_OK,
                        msg: String::new(),
                        handle_id: id,
                        host: local_ip,
                        port: local_port,
                    }),
                    &self.sched,
                )
                .is_err()
                {
                    self.close_udp_by_id(id);
                }
            }
        }

        fn cmd_udp_send(
            &mut self,
            op_id: u64,
            sock_id: i64,
            host: &str,
            port: i64,
            data: Vec<u8>,
            entry: Arc<TaskOpEntry>,
        ) {
            let Some(&uptr) = self.udps.get(&sock_id) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_CLOSED,
                        msg: "UDP socket is closed".to_string(),
                        count: 0,
                    }),
                    &self.sched,
                );
                return;
            };
            // SAFETY: `uptr` is owned on the reactor thread.
            let sock = unsafe { &mut *uptr };
            if sock.closing {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_CLOSED,
                        msg: "UDP socket is closed".to_string(),
                        count: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            let addr = match parse_ip_port(host, port) {
                Ok(a) => a,
                Err((code, msg)) => {
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::Count {
                            code,
                            msg,
                            count: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
            };
            let net_ptr = self as *mut Self;
            // SAFETY: Allocate `uv_udp_send_t` and keep `data` pinned inside `Box<UdpSendCtx>` until `on_udp_send`.
            unsafe {
                let req_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_UDP_SEND);
                let req = alloc_uv_aligned::<libuv_sys2::uv_udp_send_t>(req_size);
                let mut sctx = Box::new(UdpSendCtx {
                    op_id,
                    data,
                    net: net_ptr,
                    entry: Arc::clone(&entry),
                });
                let buf = libuv_sys2::uv_buf_init(
                    sctx.data.as_mut_ptr() as *mut c_char,
                    sctx.data.len() as c_uint,
                );
                let sctx_ptr = Box::into_raw(sctx);
                libuv_sys2::uv_req_set_data(
                    req as *mut libuv_sys2::uv_req_t,
                    sctx_ptr as *mut c_void,
                );
                let rc = libuv_sys2::uv_udp_send(
                    req,
                    sock.handle,
                    &buf,
                    1,
                    addr.as_sockaddr_ptr(),
                    Some(on_udp_send),
                );
                if rc < 0 {
                    free_uv_aligned(req);
                    drop(Box::from_raw(sctx_ptr));
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::Count {
                            code: uv_error_to_io_code(rc),
                            msg: uv_error_message(rc),
                            count: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                self.cancel_targets.insert(op_id, NetCancelTarget::UdpSend);
            }
        }

        fn cmd_udp_recv(
            &mut self,
            op_id: u64,
            sock_id: i64,
            max_bytes: usize,
            entry: Arc<TaskOpEntry>,
        ) {
            let Some(&uptr) = self.udps.get(&sock_id) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Datagram {
                        code: IO_CLOSED,
                        msg: "UDP socket is closed".to_string(),
                        data: Vec::new(),
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            };
            // SAFETY: `uptr` is owned on the reactor thread.
            let sock = unsafe { &mut *uptr };
            if sock.closing {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Datagram {
                        code: IO_CLOSED,
                        msg: "UDP socket is closed".to_string(),
                        data: Vec::new(),
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
                return;
            }
            if let Some(pkt) = sock.recv_queue.pop_front() {
                let full_data = pkt.data;
                let delivered = if full_data.len() > max_bytes {
                    full_data[..max_bytes].to_vec()
                } else {
                    full_data.clone()
                };
                if try_complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Datagram {
                        code: IO_OK,
                        msg: String::new(),
                        data: delivered,
                        host: pkt.host.clone(),
                        port: pkt.port,
                    }),
                    &self.sched,
                )
                .is_err()
                {
                    sock.recv_queue.push_front(UdpPacket {
                        data: full_data,
                        host: pkt.host,
                        port: pkt.port,
                    });
                }
                return;
            }
            if let Some((code, msg)) = sock.recv_err.take() {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Datagram {
                        code,
                        msg,
                        data: Vec::new(),
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            }
            if !sock.receiving {
                // SAFETY: Start async datagram receive on `sock.handle`.
                let rc = unsafe {
                    libuv_sys2::uv_udp_recv_start(
                        sock.handle,
                        Some(on_net_alloc),
                        Some(on_udp_recv),
                    )
                };
                if rc < 0 {
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::Datagram {
                            code: uv_error_to_io_code(rc),
                            msg: uv_error_message(rc),
                            data: Vec::new(),
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                    return;
                }
                sock.receiving = true;
            }
            sock.recv_waiters.push_back((op_id, max_bytes, entry));
            self.cancel_targets
                .insert(op_id, NetCancelTarget::UdpRecv(sock_id));
        }

        fn close_udp_by_id(&mut self, sock_id: i64) {
            let Some(uptr) = self.udps.remove(&sock_id) else {
                return;
            };
            let _ = self.registry.mark_closed(sock_id, NetHandleKind::UdpSocket);
            // SAFETY: `uptr` was removed from `self.udps` and is owned on the reactor thread.
            unsafe {
                let sock = &mut *uptr;
                sock.closing = true;
                stop_udp_receiving(sock);
                for (op_id, _, waiter) in sock.recv_waiters.drain(..) {
                    self.cancel_targets.remove(&op_id);
                    complete_entry(
                        &waiter,
                        TaskOpResult::Net(NetOutcome::Datagram {
                            code: IO_CLOSED,
                            msg: "UDP socket closed".to_string(),
                            data: Vec::new(),
                            host: String::new(),
                            port: 0,
                        }),
                        &self.sched,
                    );
                }
                libuv_sys2::uv_close(
                    sock.handle as *mut libuv_sys2::uv_handle_t,
                    Some(on_udp_close),
                );
            }
        }

        // -- DNS Resolution (`uv_getaddrinfo`) ------------------------------------

        fn cmd_resolve_host(&mut self, op_id: u64, host: &str, entry: Arc<TaskOpEntry>) {
            let trimmed = host.trim();
            if trimmed.is_empty() {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Hosts {
                        code: IO_INVALID_INPUT,
                        msg: "hostname must not be empty".to_string(),
                        addrs: Vec::new(),
                    }),
                    &self.sched,
                );
                return;
            }
            let Ok(c_host) = CString::new(trimmed) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::Hosts {
                        code: IO_INVALID_INPUT,
                        msg: "hostname contains interior NUL byte".to_string(),
                        addrs: Vec::new(),
                    }),
                    &self.sched,
                );
                return;
            };
            let net_ptr = self as *mut Self;
            // SAFETY: Allocate `uv_getaddrinfo_t` via `alloc_uv_aligned` and launch async resolution.
            unsafe {
                let req_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_GETADDRINFO);
                let req = alloc_uv_aligned::<libuv_sys2::uv_getaddrinfo_t>(req_size);
                let dctx_ptr = Box::into_raw(Box::new(DnsCtx {
                    op_id,
                    net: net_ptr,
                    entry: Arc::clone(&entry),
                }));
                libuv_sys2::uv_req_set_data(
                    req as *mut libuv_sys2::uv_req_t,
                    dctx_ptr as *mut c_void,
                );
                let rc = libuv_sys2::uv_getaddrinfo(
                    self.loop_ptr,
                    req,
                    Some(on_getaddrinfo),
                    c_host.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                );
                if rc < 0 {
                    free_uv_aligned(req);
                    drop(Box::from_raw(dctx_ptr));
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::Hosts {
                            code: uv_error_to_io_code(rc),
                            msg: uv_error_message(rc),
                            addrs: Vec::new(),
                        }),
                        &self.sched,
                    );
                    return;
                }
                self.cancel_targets.insert(op_id, NetCancelTarget::Dns(req));
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Stream / UDP helper functions & libuv C callbacks
    // ---------------------------------------------------------------------------

    fn complete_read_closed(
        entry: &Arc<TaskOpEntry>,
        mode: &TcpReadMode,
        sched: &Weak<SchedulerInner>,
    ) {
        let out = match mode {
            TcpReadMode::Bytes(_) => NetOutcome::Bytes {
                code: IO_CLOSED,
                msg: "TCP stream is closed".to_string(),
                data: Vec::new(),
            },
            TcpReadMode::Line => NetOutcome::Line {
                code: IO_CLOSED,
                msg: "TCP stream is closed".to_string(),
                line: None,
            },
        };
        complete_entry(entry, TaskOpResult::Net(out), sched);
    }

    fn stop_stream_reading(stream: &mut TcpStreamCtx) {
        if stream.reading {
            stream.reading = false;
            // SAFETY: `stream.handle` is a valid `uv_tcp_t` on the reactor thread.
            unsafe {
                let _ = libuv_sys2::uv_read_stop(stream.handle as *mut libuv_sys2::uv_stream_t);
            }
        }
    }

    fn stop_udp_receiving(sock: &mut UdpSocketCtx) {
        if sock.receiving {
            sock.receiving = false;
            // SAFETY: `sock.handle` is a valid `uv_udp_t` on the reactor thread.
            unsafe {
                let _ = libuv_sys2::uv_udp_recv_stop(sock.handle);
            }
        }
    }

    /// Attempt to complete `stream.pending_read` from `stream.read_buf` / `eof` / `read_err`.
    /// Returns `true` if `pending_read` was satisfied and consumed.
    fn try_satisfy_tcp_read(stream: &mut TcpStreamCtx, sched: &Weak<SchedulerInner>) -> bool {
        let Some(pr_ref) = stream.pending_read.as_ref() else {
            return false;
        };
        let entry = Arc::clone(&pr_ref.entry);
        let op_id = pr_ref.op_id;
        let mode_is_bytes = match pr_ref.mode {
            TcpReadMode::Bytes(n) => Some(n),
            TcpReadMode::Line => None,
        };

        // Lock `entry.state` BEFORE draining `stream.read_buf` / `stream.read_err` so a
        // cancelled `read*` operation can never discard buffered stream data.
        let mut st = entry.state.lock();
        if matches!(*st, TaskOpState::Cancelled) {
            drop(st);
            stream.pending_read = None;
            // SAFETY: `stream.net` outlives all handles on the reactor thread.
            unsafe {
                (*stream.net).cancel_targets.remove(&op_id);
            }
            stop_stream_reading(stream);
            return false;
        }

        let outcome = match mode_is_bytes {
            Some(max_bytes) => {
                if max_bytes == 0 {
                    Some(NetOutcome::Bytes {
                        code: IO_OK,
                        msg: String::new(),
                        data: Vec::new(),
                    })
                } else if !stream.read_buf.is_empty() {
                    let take_n = max_bytes.min(stream.read_buf.len());
                    let data: Vec<u8> = stream.read_buf.drain(..take_n).collect();
                    Some(NetOutcome::Bytes {
                        code: IO_OK,
                        msg: String::new(),
                        data,
                    })
                } else if stream.eof {
                    Some(NetOutcome::Bytes {
                        code: IO_OK,
                        msg: String::new(),
                        data: Vec::new(),
                    })
                } else if let Some((code, msg)) = stream.read_err.take() {
                    Some(NetOutcome::Bytes {
                        code,
                        msg,
                        data: Vec::new(),
                    })
                } else {
                    None
                }
            }
            None => {
                if let Some(nl_pos) = stream.read_buf.iter().position(|&b| b == b'\n') {
                    let mut raw: Vec<u8> = stream.read_buf.drain(..=nl_pos).collect();
                    if raw.last() == Some(&b'\n') {
                        raw.pop();
                    }
                    if raw.last() == Some(&b'\r') {
                        raw.pop();
                    }
                    let line = String::from_utf8(raw)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                    Some(NetOutcome::Line {
                        code: IO_OK,
                        msg: String::new(),
                        line: Some(line),
                    })
                } else if stream.eof {
                    if !stream.read_buf.is_empty() {
                        let mut raw: Vec<u8> = stream.read_buf.drain(..).collect();
                        if raw.last() == Some(&b'\r') {
                            raw.pop();
                        }
                        let line = String::from_utf8(raw)
                            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                        Some(NetOutcome::Line {
                            code: IO_OK,
                            msg: String::new(),
                            line: Some(line),
                        })
                    } else {
                        Some(NetOutcome::Line {
                            code: IO_OK,
                            msg: String::new(),
                            line: None,
                        })
                    }
                } else if let Some((code, msg)) = stream.read_err.take() {
                    Some(NetOutcome::Line {
                        code,
                        msg,
                        line: None,
                    })
                } else {
                    None
                }
            }
        };

        if let Some(out) = outcome {
            *st = TaskOpState::Ready(TaskOpResult::Net(out));
            drop(st);
            stream.pending_read = None;
            // SAFETY: `stream.net` outlives all handles on the reactor thread.
            unsafe {
                (*stream.net).cancel_targets.remove(&op_id);
            }
            if entry
                .io_accounted
                .swap(false, std::sync::atomic::Ordering::AcqRel)
                && let Some(s) = sched.upgrade()
            {
                let _ = s.wake_from_reactor(entry.task_id);
                s.dec_active_io();
            }
            if stream.read_buf.len() >= MAX_BUFFERED_TCP_READ_BYTES || stream.eof {
                stop_stream_reading(stream);
            }
            true
        } else {
            false
        }
    }

    unsafe extern "C" fn on_bare_uv_close(handle: *mut libuv_sys2::uv_handle_t) {
        // SAFETY: `uv_close` finished on an un-registered handle allocated with `alloc_uv_aligned`.
        unsafe { free_uv_aligned(handle) };
    }

    unsafe extern "C" fn on_listener_close(handle: *mut libuv_sys2::uv_handle_t) {
        // SAFETY: `handle` data points to `Box<TcpListenerCtx>` owned on the reactor thread.
        unsafe {
            let lptr = libuv_sys2::uv_handle_get_data(handle) as *mut TcpListenerCtx;
            free_uv_aligned(handle);
            if !lptr.is_null() {
                drop(Box::from_raw(lptr));
            }
        }
    }

    unsafe extern "C" fn on_stream_close(handle: *mut libuv_sys2::uv_handle_t) {
        // SAFETY: `handle` data points to `Box<TcpStreamCtx>` owned on the reactor thread.
        unsafe {
            let sptr = libuv_sys2::uv_handle_get_data(handle) as *mut TcpStreamCtx;
            free_uv_aligned(handle);
            if !sptr.is_null() {
                drop(Box::from_raw(sptr));
            }
        }
    }

    unsafe extern "C" fn on_udp_close(handle: *mut libuv_sys2::uv_handle_t) {
        // SAFETY: `handle` data points to `Box<UdpSocketCtx>` owned on the reactor thread.
        unsafe {
            let uptr = libuv_sys2::uv_handle_get_data(handle) as *mut UdpSocketCtx;
            free_uv_aligned(handle);
            if !uptr.is_null() {
                drop(Box::from_raw(uptr));
            }
        }
    }

    unsafe extern "C" fn on_net_alloc(
        _handle: *mut libuv_sys2::uv_handle_t,
        suggested_size: usize,
        buf: *mut libuv_sys2::uv_buf_t,
    ) {
        let cap = suggested_size.clamp(4096, 65536);
        let mut vec = Vec::<u8>::with_capacity(cap);
        let ptr = vec.as_mut_ptr() as *mut c_char;
        std::mem::forget(vec);
        // SAFETY: `buf` is a valid output pointer provided by libuv before read/recv callback.
        unsafe {
            (*buf).base = ptr;
            (*buf).len = cap;
        }
    }

    unsafe extern "C" fn on_tcp_connection(server: *mut libuv_sys2::uv_stream_t, status: c_int) {
        // SAFETY: `server` is a listening `uv_tcp_t` whose data points to `TcpListenerCtx`.
        unsafe {
            let lptr = libuv_sys2::uv_handle_get_data(server as *mut libuv_sys2::uv_handle_t)
                as *mut TcpListenerCtx;
            if lptr.is_null() {
                return;
            }
            let listener = &mut *lptr;
            if listener.closing {
                return;
            }
            let net = &mut *listener.net;
            if net.shutting_down {
                return;
            }
            if status < 0 {
                if let Some((op_id, waiter)) = listener.accept_waiters.pop_front() {
                    net.cancel_targets.remove(&op_id);
                    complete_entry(
                        &waiter,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(status),
                            msg: uv_error_message(status),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &net.sched,
                    );
                }
                return;
            }

            let tcp_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_TCP);
            let client = alloc_uv_aligned::<libuv_sys2::uv_tcp_t>(tcp_size);
            if libuv_sys2::uv_tcp_init(net.loop_ptr, client) < 0 {
                free_uv_aligned(client);
                return;
            }
            let rc_acc = libuv_sys2::uv_accept(server, client as *mut libuv_sys2::uv_stream_t);
            if rc_acc < 0 {
                libuv_sys2::uv_close(
                    client as *mut libuv_sys2::uv_handle_t,
                    Some(on_bare_uv_close),
                );
                if let Some((op_id, waiter)) = listener.accept_waiters.pop_front() {
                    net.cancel_targets.remove(&op_id);
                    complete_entry(
                        &waiter,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: uv_error_to_io_code(rc_acc),
                            msg: uv_error_message(rc_acc),
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &net.sched,
                    );
                }
                return;
            }

            match net.register_connected_stream(client) {
                Ok((stream_id, _, (peer_ip, peer_port))) => {
                    let mut delivered = false;
                    while let Some((op_id, waiter)) = listener.accept_waiters.pop_front() {
                        net.cancel_targets.remove(&op_id);
                        if try_complete_entry(
                            &waiter,
                            TaskOpResult::Net(NetOutcome::HandleAddr {
                                code: IO_OK,
                                msg: String::new(),
                                handle_id: stream_id,
                                host: peer_ip.clone(),
                                port: peer_port,
                            }),
                            &net.sched,
                        )
                        .is_ok()
                        {
                            delivered = true;
                            break;
                        }
                    }
                    if !delivered {
                        listener.backlog.push_back((stream_id, peer_ip, peer_port));
                    }
                }
                Err((code, msg)) => {
                    libuv_sys2::uv_close(
                        client as *mut libuv_sys2::uv_handle_t,
                        Some(on_bare_uv_close),
                    );
                    if let Some((op_id, waiter)) = listener.accept_waiters.pop_front() {
                        net.cancel_targets.remove(&op_id);
                        complete_entry(
                            &waiter,
                            TaskOpResult::Net(NetOutcome::HandleAddr {
                                code,
                                msg,
                                handle_id: -1,
                                host: String::new(),
                                port: 0,
                            }),
                            &net.sched,
                        );
                    }
                }
            }
        }
    }

    unsafe fn cancel_tcp_connect_ctx(cptr: *mut TcpConnectCtx) {
        // SAFETY: Called on the reactor thread while `cptr` is still alive.
        unsafe {
            let ctx = &mut *cptr;
            ctx.cancelled = true;
            if !ctx.dns_req.is_null() {
                let _ = libuv_sys2::uv_cancel(ctx.dns_req as *mut libuv_sys2::uv_req_t);
            }
            if !ctx.tcp_handle.is_null() && !ctx.tcp_closing {
                ctx.tcp_closing = true;
                libuv_sys2::uv_close(
                    ctx.tcp_handle as *mut libuv_sys2::uv_handle_t,
                    Some(on_connect_tcp_close),
                );
            }
        }
    }

    unsafe fn maybe_reclaim_connect_ctx(cptr: *mut TcpConnectCtx) {
        // SAFETY: Called on the reactor thread after either `on_tcp_connect` or `on_connect_tcp_close`.
        unsafe {
            let ctx = &*cptr;
            if ctx.req_done && ctx.dns_req.is_null() && ctx.tcp_handle.is_null() {
                drop(Box::from_raw(cptr));
            }
        }
    }

    unsafe extern "C" fn on_connect_tcp_close(handle: *mut libuv_sys2::uv_handle_t) {
        // SAFETY: `handle` belongs to a failed or cancelled `TcpConnectCtx`.
        unsafe {
            let cptr = libuv_sys2::uv_handle_get_data(handle) as *mut TcpConnectCtx;
            free_uv_aligned(handle);
            if !cptr.is_null() {
                (*cptr).tcp_handle = ptr::null_mut();
                maybe_reclaim_connect_ctx(cptr);
            }
        }
    }

    unsafe extern "C" fn on_connect_dns_resolved(
        req: *mut libuv_sys2::uv_getaddrinfo_t,
        status: c_int,
        res: *mut libuv_sys2::addrinfo,
    ) {
        // SAFETY: `req` data points to `TcpConnectCtx` on the reactor thread.
        unsafe {
            let cptr =
                libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut TcpConnectCtx;
            free_uv_aligned(req);
            if cptr.is_null() {
                if !res.is_null() {
                    libuv_sys2::uv_freeaddrinfo(res);
                }
                return;
            }
            let ctx = &mut *cptr;
            ctx.dns_req = ptr::null_mut();
            let net = &mut *ctx.net;

            if ctx.cancelled || net.shutting_down {
                if !res.is_null() {
                    libuv_sys2::uv_freeaddrinfo(res);
                }
                ctx.req_done = true;
                maybe_reclaim_connect_ctx(cptr);
                return;
            }

            if status < 0 || res.is_null() {
                net.cancel_targets.remove(&ctx.op_id);
                if !res.is_null() {
                    libuv_sys2::uv_freeaddrinfo(res);
                }
                let err_code = if status < 0 {
                    uv_error_to_io_code(status)
                } else {
                    IO_OTHER
                };
                let err_msg = if status < 0 {
                    uv_error_message(status)
                } else {
                    "DNS returned no addresses".to_string()
                };
                complete_entry(
                    &ctx.entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: err_code,
                        msg: err_msg,
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &net.sched,
                );
                ctx.req_done = true;
                maybe_reclaim_connect_ctx(cptr);
                return;
            }

            let mut target_addr: Option<SockAddrBuf> = None;
            let mut cur = res;
            while !cur.is_null() {
                if let Some((ip, _)) = extract_sockaddr((*cur).ai_addr)
                    && let Ok(parsed) = parse_ip_port(&ip, ctx.port)
                {
                    target_addr = Some(parsed);
                    break;
                }
                cur = (*cur).ai_next;
            }
            libuv_sys2::uv_freeaddrinfo(res);

            let Some(addr) = target_addr else {
                net.cancel_targets.remove(&ctx.op_id);
                complete_entry(
                    &ctx.entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: IO_OTHER,
                        msg: "no usable IPv4/IPv6 address resolved".to_string(),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &net.sched,
                );
                ctx.req_done = true;
                maybe_reclaim_connect_ctx(cptr);
                return;
            };

            let tcp_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_TCP);
            let tcp_handle = alloc_uv_aligned::<libuv_sys2::uv_tcp_t>(tcp_size);
            let rc_init = libuv_sys2::uv_tcp_init(net.loop_ptr, tcp_handle);
            if rc_init < 0 {
                free_uv_aligned(tcp_handle);
                net.cancel_targets.remove(&ctx.op_id);
                complete_entry(
                    &ctx.entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: uv_error_to_io_code(rc_init),
                        msg: uv_error_message(rc_init),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &net.sched,
                );
                ctx.req_done = true;
                maybe_reclaim_connect_ctx(cptr);
                return;
            }
            let creq_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_CONNECT);
            let connect_req = alloc_uv_aligned::<libuv_sys2::uv_connect_t>(creq_size);
            ctx.tcp_handle = tcp_handle;
            ctx.connect_req = connect_req;
            libuv_sys2::uv_handle_set_data(
                tcp_handle as *mut libuv_sys2::uv_handle_t,
                cptr as *mut c_void,
            );
            libuv_sys2::uv_req_set_data(
                connect_req as *mut libuv_sys2::uv_req_t,
                cptr as *mut c_void,
            );
            let rc_conn = libuv_sys2::uv_tcp_connect(
                connect_req,
                tcp_handle,
                addr.as_sockaddr_ptr(),
                Some(on_tcp_connect),
            );
            if rc_conn < 0 {
                net.cancel_targets.remove(&ctx.op_id);
                free_uv_aligned(connect_req);
                ctx.connect_req = ptr::null_mut();
                ctx.req_done = true;
                ctx.tcp_closing = true;
                libuv_sys2::uv_close(
                    tcp_handle as *mut libuv_sys2::uv_handle_t,
                    Some(on_connect_tcp_close),
                );
                complete_entry(
                    &ctx.entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: uv_error_to_io_code(rc_conn),
                        msg: uv_error_message(rc_conn),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &net.sched,
                );
            }
        }
    }

    unsafe extern "C" fn on_tcp_connect(req: *mut libuv_sys2::uv_connect_t, status: c_int) {
        // SAFETY: `req` data points to `TcpConnectCtx` on the reactor thread.
        unsafe {
            let cptr =
                libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut TcpConnectCtx;
            free_uv_aligned(req);
            if cptr.is_null() {
                return;
            }
            let ctx = &mut *cptr;
            ctx.connect_req = ptr::null_mut();
            ctx.req_done = true;
            let net = &mut *ctx.net;
            net.cancel_targets.remove(&ctx.op_id);

            if ctx.cancelled || net.shutting_down {
                if !ctx.tcp_handle.is_null() && !ctx.tcp_closing {
                    ctx.tcp_closing = true;
                    libuv_sys2::uv_close(
                        ctx.tcp_handle as *mut libuv_sys2::uv_handle_t,
                        Some(on_connect_tcp_close),
                    );
                } else {
                    maybe_reclaim_connect_ctx(cptr);
                }
                return;
            }

            if status < 0 {
                if !ctx.tcp_handle.is_null() && !ctx.tcp_closing {
                    ctx.tcp_closing = true;
                    libuv_sys2::uv_close(
                        ctx.tcp_handle as *mut libuv_sys2::uv_handle_t,
                        Some(on_connect_tcp_close),
                    );
                }
                complete_entry(
                    &ctx.entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: uv_error_to_io_code(status),
                        msg: uv_error_message(status),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &net.sched,
                );
                maybe_reclaim_connect_ctx(cptr);
                return;
            }

            let handle = ctx.tcp_handle;
            ctx.tcp_handle = ptr::null_mut();
            match net.register_connected_stream(handle) {
                Ok((stream_id, (local_ip, local_port), _)) => {
                    if try_complete_entry(
                        &ctx.entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: IO_OK,
                            msg: String::new(),
                            handle_id: stream_id,
                            host: local_ip,
                            port: local_port,
                        }),
                        &net.sched,
                    )
                    .is_err()
                    {
                        net.close_stream_by_id(stream_id);
                    }
                }
                Err((code, msg)) => {
                    libuv_sys2::uv_close(
                        handle as *mut libuv_sys2::uv_handle_t,
                        Some(on_bare_uv_close),
                    );
                    complete_entry(
                        &ctx.entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code,
                            msg,
                            handle_id: -1,
                            host: String::new(),
                            port: 0,
                        }),
                        &net.sched,
                    );
                }
            }
            maybe_reclaim_connect_ctx(cptr);
        }
    }

    unsafe extern "C" fn on_tcp_read(
        handle: *mut libuv_sys2::uv_stream_t,
        nread: isize,
        buf: *const libuv_sys2::uv_buf_t,
    ) {
        // SAFETY: Reclaim `buf` allocated by `on_net_alloc` and feed `TcpStreamCtx`.
        unsafe {
            let mut chunk = Vec::new();
            if !buf.is_null() && !(*buf).base.is_null() && (*buf).len > 0 {
                let len = if nread > 0 { nread as usize } else { 0 };
                let owned = Vec::from_raw_parts((*buf).base as *mut u8, len, (*buf).len);
                if len > 0 {
                    chunk = owned;
                }
            }
            let sptr = libuv_sys2::uv_handle_get_data(handle as *mut libuv_sys2::uv_handle_t)
                as *mut TcpStreamCtx;
            if sptr.is_null() {
                return;
            }
            let stream = &mut *sptr;
            if stream.closing {
                return;
            }
            if !chunk.is_empty() {
                stream.read_buf.extend(chunk);
            }
            if nread < 0 {
                if nread as c_int == libuv_sys2::uv_errno_t_UV_EOF {
                    stream.eof = true;
                } else {
                    stream.read_err = Some((
                        uv_error_to_io_code(nread as c_int),
                        uv_error_message(nread as c_int),
                    ));
                }
                stop_stream_reading(stream);
            }
            let net = &mut *stream.net;
            let _ = try_satisfy_tcp_read(stream, &net.sched);
            if stream.pending_read.is_none() && stream.read_buf.len() >= MAX_BUFFERED_TCP_READ_BYTES
            {
                stop_stream_reading(stream);
            }
        }
    }

    unsafe extern "C" fn on_tcp_write(req: *mut libuv_sys2::uv_write_t, status: c_int) {
        // SAFETY: `req` data points to `Box<TcpWriteCtx>` owned on the reactor thread.
        unsafe {
            let wptr =
                libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut TcpWriteCtx;
            free_uv_aligned(req);
            if wptr.is_null() {
                return;
            }
            let wctx = Box::from_raw(wptr);
            let net = &mut *wctx.net;
            net.cancel_targets.remove(&wctx.op_id);
            let out = if status < 0 {
                NetOutcome::Count {
                    code: uv_error_to_io_code(status),
                    msg: uv_error_message(status),
                    count: 0,
                }
            } else {
                NetOutcome::Count {
                    code: IO_OK,
                    msg: String::new(),
                    count: wctx.data.len() as i64,
                }
            };
            complete_entry(&wctx.entry, TaskOpResult::Net(out), &net.sched);
        }
    }

    unsafe extern "C" fn on_udp_send(req: *mut libuv_sys2::uv_udp_send_t, status: c_int) {
        // SAFETY: `req` data points to `Box<UdpSendCtx>` owned on the reactor thread.
        unsafe {
            let sptr =
                libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut UdpSendCtx;
            free_uv_aligned(req);
            if sptr.is_null() {
                return;
            }
            let sctx = Box::from_raw(sptr);
            let net = &mut *sctx.net;
            net.cancel_targets.remove(&sctx.op_id);
            let out = if status < 0 {
                NetOutcome::Count {
                    code: uv_error_to_io_code(status),
                    msg: uv_error_message(status),
                    count: 0,
                }
            } else {
                NetOutcome::Count {
                    code: IO_OK,
                    msg: String::new(),
                    count: sctx.data.len() as i64,
                }
            };
            complete_entry(&sctx.entry, TaskOpResult::Net(out), &net.sched);
        }
    }

    unsafe extern "C" fn on_udp_recv(
        handle: *mut libuv_sys2::uv_udp_t,
        nread: isize,
        buf: *const libuv_sys2::uv_buf_t,
        addr: *const libuv_sys2::sockaddr,
        _flags: c_uint,
    ) {
        // SAFETY: Reclaim `buf` allocated by `on_net_alloc` and deliver datagram to `UdpSocketCtx`.
        unsafe {
            let mut payload = Vec::new();
            if !buf.is_null() && !(*buf).base.is_null() && (*buf).len > 0 {
                let len = if nread > 0 { nread as usize } else { 0 };
                let owned = Vec::from_raw_parts((*buf).base as *mut u8, len, (*buf).len);
                if len > 0 {
                    payload = owned;
                }
            }
            let uptr = libuv_sys2::uv_handle_get_data(handle as *mut libuv_sys2::uv_handle_t)
                as *mut UdpSocketCtx;
            if uptr.is_null() {
                return;
            }
            let sock = &mut *uptr;
            if sock.closing {
                return;
            }
            let net = &mut *sock.net;
            if nread < 0 {
                let code = uv_error_to_io_code(nread as c_int);
                let msg = uv_error_message(nread as c_int);
                if let Some((op_id, _, waiter)) = sock.recv_waiters.pop_front() {
                    net.cancel_targets.remove(&op_id);
                    complete_entry(
                        &waiter,
                        TaskOpResult::Net(NetOutcome::Datagram {
                            code,
                            msg,
                            data: Vec::new(),
                            host: String::new(),
                            port: 0,
                        }),
                        &net.sched,
                    );
                } else {
                    sock.recv_err = Some((code, msg));
                }
                if sock.recv_waiters.is_empty() {
                    stop_udp_receiving(sock);
                }
                return;
            }
            if nread == 0 && addr.is_null() {
                // libuv empty end-of-batch notification, not an actual datagram.
                return;
            }
            let (peer_ip, peer_port) = extract_sockaddr(addr).unwrap_or_else(|| (String::new(), 0));
            let mut delivered = false;
            while let Some((op_id, max_bytes, waiter)) = sock.recv_waiters.pop_front() {
                net.cancel_targets.remove(&op_id);
                let data = if payload.len() > max_bytes {
                    payload[..max_bytes].to_vec()
                } else {
                    payload.clone()
                };
                if try_complete_entry(
                    &waiter,
                    TaskOpResult::Net(NetOutcome::Datagram {
                        code: IO_OK,
                        msg: String::new(),
                        data,
                        host: peer_ip.clone(),
                        port: peer_port,
                    }),
                    &net.sched,
                )
                .is_ok()
                {
                    delivered = true;
                    break;
                }
            }
            if delivered {
                if sock.recv_waiters.is_empty() {
                    stop_udp_receiving(sock);
                }
            } else {
                sock.recv_queue.push_back(UdpPacket {
                    data: payload,
                    host: peer_ip,
                    port: peer_port,
                });
                if sock.recv_queue.len() >= 256 {
                    stop_udp_receiving(sock);
                }
            }
        }
    }

    unsafe extern "C" fn on_getaddrinfo(
        req: *mut libuv_sys2::uv_getaddrinfo_t,
        status: c_int,
        res: *mut libuv_sys2::addrinfo,
    ) {
        // SAFETY: `req` data points to `Box<DnsCtx>` owned on the reactor thread.
        unsafe {
            let dptr = libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut DnsCtx;
            free_uv_aligned(req);
            if dptr.is_null() {
                if !res.is_null() {
                    libuv_sys2::uv_freeaddrinfo(res);
                }
                return;
            }
            let dctx = Box::from_raw(dptr);
            let net = &mut *dctx.net;
            net.cancel_targets.remove(&dctx.op_id);

            if status < 0 {
                if !res.is_null() {
                    libuv_sys2::uv_freeaddrinfo(res);
                }
                complete_entry(
                    &dctx.entry,
                    TaskOpResult::Net(NetOutcome::Hosts {
                        code: uv_error_to_io_code(status),
                        msg: uv_error_message(status),
                        addrs: Vec::new(),
                    }),
                    &net.sched,
                );
                return;
            }

            let mut addrs = Vec::new();
            let mut cur = res;
            while !cur.is_null() {
                if let Some((ip, _)) = extract_sockaddr((*cur).ai_addr)
                    && !addrs.contains(&ip)
                {
                    addrs.push(ip);
                }
                cur = (*cur).ai_next;
            }
            if !res.is_null() {
                libuv_sys2::uv_freeaddrinfo(res);
            }
            complete_entry(
                &dctx.entry,
                TaskOpResult::Net(NetOutcome::Hosts {
                    code: IO_OK,
                    msg: String::new(),
                    addrs,
                }),
                &net.sched,
            );
        }
    }
}
