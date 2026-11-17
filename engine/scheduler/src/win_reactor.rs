//! Native Windows (`Win10 1709+`) asynchronous I/O reactor.
//!
//! Drives millisecond timers, cancellable child processes (with Win32 Job Object tree
//! termination), threadpool work items, DNS resolution, and non-blocking TCP/UDP sockets
//! via Winsock2 `WSAPoll` (`ws2_32.dll`, available since Windows Vista).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::os::windows::io::AsRawSocket;
use std::process::{Command, Stdio};
use std::ptr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use runtime::FastHashMap;

use crate::SchedulerInner;
use crate::reactor::{
    BoxedWorkFn, Doorbell, IO_ALREADY_EXISTS, IO_BROKEN_PIPE, IO_INVALID_INPUT, IO_NOT_FOUND,
    IO_OK, IO_OTHER, IO_PERMISSION_DENIED, IO_UNEXPECTED_EOF, ProcessOutcome, ProcessSpec,
    ReactorCmd, TaskOpEntry, TaskOpResult, TaskOpState, complete_entry,
};
use crate::reactor_net::{
    MAX_BUFFERED_TCP_READ_BYTES, NetCmd, NetHandleKind, NetHandleMeta, NetOutcome, NetRegistry,
    TcpReadMode, normalize_host,
};

const IO_CLOSED: i64 = 6;
const WORKER_POOL_THREADS: usize = 4;

// ---------------------------------------------------------------------------
// Win32 (`kernel32.dll`) & Winsock2 (`ws2_32.dll`) FFI — Windows Vista / Win8+
// ---------------------------------------------------------------------------

mod win_ffi {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;

    pub const PROCESS_TERMINATE: u32 = 0x0001;
    pub const PROCESS_SET_QUOTA: u32 = 0x0100;
    pub const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    pub const INVALID_HANDLE_VALUE: isize = -1;
    pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
    pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    pub const POLLRDNORM: i16 = 0x0100;
    pub const POLLWRNORM: i16 = 0x0010;
    pub const POLLERR: i16 = 0x0001;
    pub const POLLHUP: i16 = 0x0002;
    pub const POLLNVAL: i16 = 0x0004;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ProcessEntry32W {
        pub dw_size: u32,
        pub cnt_usage: u32,
        pub th32_process_id: u32,
        pub th32_default_heap_id: usize,
        pub th32_module_id: u32,
        pub cnt_threads: u32,
        pub th32_parent_process_id: u32,
        pub pc_pri_class_base: i32,
        pub dw_flags: u32,
        pub sz_exe_file: [u16; 260],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct WsaPollFd {
        pub fd: usize,
        pub events: i16,
        pub revents: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct JobObjectBasicLimitInformation {
        pub per_process_user_time_limit: i64,
        pub per_job_user_time_limit: i64,
        pub limit_flags: u32,
        pub minimum_working_set_size: usize,
        pub maximum_working_set_size: usize,
        pub active_process_limit: u32,
        pub affinity: usize,
        pub priority_class: u32,
        pub scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct IoCounters {
        pub read_operation_count: u64,
        pub write_operation_count: u64,
        pub other_operation_count: u64,
        pub read_transfer_count: u64,
        pub write_transfer_count: u64,
        pub other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct JobObjectExtendedLimitInformation {
        pub basic_limit_information: JobObjectBasicLimitInformation,
        pub io_info: IoCounters,
        pub process_memory_limit: usize,
        pub job_memory_limit: usize,
        pub peak_process_memory_used: usize,
        pub peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
        pub fn TerminateProcess(process: Handle, exit_code: u32) -> i32;
        pub fn CloseHandle(object: Handle) -> i32;
        pub fn CreateJobObjectW(job_attributes: *mut c_void, name: *const u16) -> Handle;
        pub fn SetInformationJobObject(
            job: Handle,
            info_class: i32,
            info: *const c_void,
            info_len: u32,
        ) -> i32;
        pub fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        pub fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
        pub fn CreateToolhelp32Snapshot(flags: u32, th32_process_id: u32) -> Handle;
        pub fn Process32FirstW(snapshot: Handle, lppe: *mut ProcessEntry32W) -> i32;
        pub fn Process32NextW(snapshot: Handle, lppe: *mut ProcessEntry32W) -> i32;
    }

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        pub fn WSAPoll(fd_array: *mut WsaPollFd, fds: u32, timeout: i32) -> i32;
    }
}

/// Best-effort termination of a Windows child process and all its descendants by PID.
pub(crate) fn terminate_win_pid(root_pid: u32) {
    if root_pid == 0 {
        return;
    }
    let mut to_kill = vec![root_pid];
    // SAFETY: `CreateToolhelp32Snapshot` + `Process32FirstW`/`Process32NextW` (Win2000+)
    // enumerate `(pid, parent_pid)` pairs so grandchild processes (e.g. `cmd.exe` children)
    // are terminated together even when nested Job Objects are unavailable.
    unsafe {
        let snap = win_ffi::CreateToolhelp32Snapshot(win_ffi::TH32CS_SNAPPROCESS, 0);
        if !snap.is_null() && (snap as isize) != win_ffi::INVALID_HANDLE_VALUE {
            let mut pairs = Vec::new();
            let mut pe: win_ffi::ProcessEntry32W = std::mem::zeroed();
            pe.dw_size = std::mem::size_of::<win_ffi::ProcessEntry32W>() as u32;
            if win_ffi::Process32FirstW(snap, &raw mut pe) != 0 {
                loop {
                    pairs.push((pe.th32_process_id, pe.th32_parent_process_id));
                    pe.dw_size = std::mem::size_of::<win_ffi::ProcessEntry32W>() as u32;
                    if win_ffi::Process32NextW(snap, &raw mut pe) == 0 {
                        break;
                    }
                }
            }
            let _ = win_ffi::CloseHandle(snap);

            let mut idx = 0;
            while idx < to_kill.len() {
                let cur = to_kill[idx];
                idx += 1;
                for &(pid, ppid) in &pairs {
                    if ppid == cur && pid != 0 && !to_kill.contains(&pid) {
                        to_kill.push(pid);
                    }
                }
            }
        }

        for pid in to_kill.into_iter().rev() {
            let h = win_ffi::OpenProcess(win_ffi::PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                let _ = win_ffi::TerminateProcess(h, 1);
                let _ = win_ffi::CloseHandle(h);
            }
        }
    }
}

struct WinJobObject {
    handle: usize,
}

impl WinJobObject {
    fn create_for_pid(pid: u32) -> Option<Arc<Self>> {
        // SAFETY: Creates an anonymous Win32 Job Object configured with KILL_ON_JOB_CLOSE
        // and assigns `pid` to it so cancelling kills child + grandchild processes together.
        unsafe {
            let job = win_ffi::CreateJobObjectW(ptr::null_mut(), ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: win_ffi::JobObjectExtendedLimitInformation = std::mem::zeroed();
            info.basic_limit_information.limit_flags = win_ffi::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok_set = win_ffi::SetInformationJobObject(
                job,
                win_ffi::JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                (&raw const info).cast(),
                std::mem::size_of::<win_ffi::JobObjectExtendedLimitInformation>() as u32,
            );
            if ok_set == 0 {
                let _ = win_ffi::CloseHandle(job);
                return None;
            }
            let proc_h = win_ffi::OpenProcess(
                win_ffi::PROCESS_SET_QUOTA | win_ffi::PROCESS_TERMINATE,
                0,
                pid,
            );
            if proc_h.is_null() {
                let _ = win_ffi::CloseHandle(job);
                return None;
            }
            let ok_assign = win_ffi::AssignProcessToJobObject(job, proc_h);
            let _ = win_ffi::CloseHandle(proc_h);
            if ok_assign == 0 {
                let _ = win_ffi::CloseHandle(job);
                return None;
            }
            Some(Arc::new(Self {
                handle: job as usize,
            }))
        }
    }

    fn terminate(&self) {
        // SAFETY: `self.handle` is a valid Job Object handle until `Drop`.
        unsafe {
            let _ = win_ffi::TerminateJobObject(self.handle as win_ffi::Handle, 1);
        }
    }
}

impl Drop for WinJobObject {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is closed once on `Arc` drop.
        unsafe {
            let _ = win_ffi::CloseHandle(self.handle as win_ffi::Handle);
        }
    }
}

// ---------------------------------------------------------------------------
// Loopback UDP Reactor Waker + Internal Completion Channel
// ---------------------------------------------------------------------------

pub(crate) enum InternalEvent {
    TcpConnected {
        op_id: u64,
        entry: Arc<TaskOpEntry>,
        result: Result<TcpStream, (i64, String)>,
    },
    ProcessFinished {
        op_id: u64,
    },
}

pub(crate) struct WinWaker {
    tx: UdpSocket,
    notified: AtomicBool,
    internal: Mutex<VecDeque<InternalEvent>>,
}

impl WinWaker {
    pub(crate) fn new() -> (Arc<Self>, UdpSocket) {
        let rx = UdpSocket::bind("127.0.0.1:0").expect("failed to bind reactor waker rx socket");
        rx.set_nonblocking(true)
            .expect("failed to set reactor waker rx nonblocking");
        let rx_addr = rx.local_addr().expect("reactor waker rx local_addr");

        let tx = UdpSocket::bind("127.0.0.1:0").expect("failed to bind reactor waker tx socket");
        tx.set_nonblocking(true)
            .expect("failed to set reactor waker tx nonblocking");
        tx.connect(rx_addr)
            .expect("failed to connect reactor waker loopback socket");

        (
            Arc::new(Self {
                tx,
                notified: AtomicBool::new(false),
                internal: Mutex::new(VecDeque::new()),
            }),
            rx,
        )
    }

    #[inline]
    pub(crate) fn wake(&self) {
        if !self.notified.swap(true, Ordering::AcqRel) {
            let _ = self.tx.send(&[1u8]);
        }
    }

    fn push_internal(&self, ev: InternalEvent) {
        self.internal.lock().push_back(ev);
        self.wake();
    }
}

// ---------------------------------------------------------------------------
// Blocking Helper Pool (`QueueWork`, `ResolveHost`, `TcpConnect`)
// ---------------------------------------------------------------------------

enum HelperJob {
    Work {
        work: BoxedWorkFn,
        entry: Arc<TaskOpEntry>,
        sched: Weak<SchedulerInner>,
    },
    Resolve {
        host: String,
        entry: Arc<TaskOpEntry>,
        sched: Weak<SchedulerInner>,
    },
    Connect {
        op_id: u64,
        host: String,
        port: u16,
        entry: Arc<TaskOpEntry>,
        waker: Arc<WinWaker>,
    },
}

struct HelperQueue {
    closed: bool,
    jobs: VecDeque<HelperJob>,
}

struct WinHelperPool {
    state: Arc<(Mutex<HelperQueue>, Condvar)>,
    workers: Vec<JoinHandle<()>>,
}

impl WinHelperPool {
    fn new() -> Self {
        Self {
            state: Arc::new((
                Mutex::new(HelperQueue {
                    closed: false,
                    jobs: VecDeque::new(),
                }),
                Condvar::new(),
            )),
            workers: Vec::new(),
        }
    }

    fn submit(&mut self, job: HelperJob) {
        if self.workers.is_empty() {
            for idx in 0..WORKER_POOL_THREADS {
                let state = Arc::clone(&self.state);
                let jh = std::thread::Builder::new()
                    .name(format!("nessa-io-worker-{idx}"))
                    .spawn(move || helper_worker_loop(state))
                    .expect("failed to spawn nessa-io-worker");
                self.workers.push(jh);
            }
        }
        let (lock, cvar) = &*self.state;
        let mut q = lock.lock();
        if !q.closed {
            q.jobs.push_back(job);
            cvar.notify_one();
        }
    }

    fn shutdown(&mut self) {
        {
            let (lock, cvar) = &*self.state;
            let mut q = lock.lock();
            q.closed = true;
            q.jobs.clear();
            cvar.notify_all();
        }
        for jh in self.workers.drain(..) {
            let _ = jh.join();
        }
    }
}

fn helper_worker_loop(state: Arc<(Mutex<HelperQueue>, Condvar)>) {
    let (lock, cvar) = &*state;
    loop {
        let job = {
            let mut q = lock.lock();
            while q.jobs.is_empty() && !q.closed {
                cvar.wait(&mut q);
            }
            if q.closed && q.jobs.is_empty() {
                return;
            }
            q.jobs.pop_front()
        };
        let Some(job) = job else { continue };
        match job {
            HelperJob::Work { work, entry, sched } => {
                if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
                    continue;
                }
                let out = work();
                complete_entry(&entry, TaskOpResult::Work(out), &sched);
            }
            HelperJob::Resolve { host, entry, sched } => {
                if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
                    continue;
                }
                let outcome = resolve_host_blocking(&host);
                complete_entry(&entry, TaskOpResult::Net(outcome), &sched);
            }
            HelperJob::Connect {
                op_id,
                host,
                port,
                entry,
                waker,
            } => {
                if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
                    continue;
                }
                let result = connect_tcp_blocking(&host, port);
                waker.push_internal(InternalEvent::TcpConnected {
                    op_id,
                    entry,
                    result,
                });
            }
        }
    }
}

fn io_error_to_code(err: &std::io::Error) -> i64 {
    match err.kind() {
        ErrorKind::NotFound => IO_NOT_FOUND,
        ErrorKind::PermissionDenied => IO_PERMISSION_DENIED,
        ErrorKind::AlreadyExists | ErrorKind::AddrInUse => IO_ALREADY_EXISTS,
        ErrorKind::InvalidInput | ErrorKind::InvalidData => IO_INVALID_INPUT,
        ErrorKind::UnexpectedEof => IO_UNEXPECTED_EOF,
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
            IO_BROKEN_PIPE
        }
        _ => IO_OTHER,
    }
}

fn parse_socket_addr(host: &str, port: i64) -> Result<SocketAddr, (i64, String)> {
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
    if norm.contains('\0') {
        return Err((
            IO_INVALID_INPUT,
            "host contains interior NUL byte".to_string(),
        ));
    }
    let ip = IpAddr::from_str(norm).map_err(|_| {
        (
            IO_INVALID_INPUT,
            format!("invalid IPv4/IPv6 address `{host}`"),
        )
    })?;
    Ok(SocketAddr::new(ip, port as u16))
}

fn format_sock_addr(addr: SocketAddr) -> (String, i64) {
    (addr.ip().to_string(), i64::from(addr.port()))
}

fn resolve_host_blocking(host: &str) -> NetOutcome {
    let norm = host.trim();
    if let Ok(ip) = IpAddr::from_str(norm) {
        return NetOutcome::Hosts {
            code: IO_OK,
            msg: String::new(),
            addrs: vec![ip.to_string()],
        };
    }
    match (norm, 0u16).to_socket_addrs() {
        Ok(iter) => {
            let mut addrs = Vec::new();
            for sa in iter {
                let s = sa.ip().to_string();
                if !addrs.contains(&s) {
                    addrs.push(s);
                }
            }
            if addrs.is_empty() && norm.eq_ignore_ascii_case("localhost") {
                addrs.push("127.0.0.1".to_string());
            }
            NetOutcome::Hosts {
                code: IO_OK,
                msg: String::new(),
                addrs,
            }
        }
        Err(e) => {
            if norm.eq_ignore_ascii_case("localhost") {
                NetOutcome::Hosts {
                    code: IO_OK,
                    msg: String::new(),
                    addrs: vec!["127.0.0.1".to_string()],
                }
            } else {
                NetOutcome::Hosts {
                    code: io_error_to_code(&e),
                    msg: e.to_string(),
                    addrs: Vec::new(),
                }
            }
        }
    }
}

fn connect_tcp_blocking(host: &str, port: u16) -> Result<TcpStream, (i64, String)> {
    let norm = normalize_host(host);
    if let Ok(ip) = IpAddr::from_str(norm) {
        let stream = TcpStream::connect(SocketAddr::new(ip, port))
            .map_err(|e| (io_error_to_code(&e), e.to_string()))?;
        stream
            .set_nonblocking(true)
            .map_err(|e| (io_error_to_code(&e), e.to_string()))?;
        return Ok(stream);
    }
    let addrs: Vec<SocketAddr> = (norm, port)
        .to_socket_addrs()
        .map_err(|e| (io_error_to_code(&e), e.to_string()))?
        .collect();
    if addrs.is_empty() {
        return Err((IO_NOT_FOUND, format!("no addresses resolved for `{host}`")));
    }
    let mut last_err = None;
    for addr in addrs {
        match TcpStream::connect(addr) {
            Ok(stream) => {
                stream
                    .set_nonblocking(true)
                    .map_err(|e| (io_error_to_code(&e), e.to_string()))?;
                return Ok(stream);
            }
            Err(e) => last_err = Some(e),
        }
    }
    let e = last_err.expect("addrs verified non-empty");
    Err((io_error_to_code(&e), e.to_string()))
}

// ---------------------------------------------------------------------------
// Per-Socket Non-Blocking Reactor State
// ---------------------------------------------------------------------------

struct TcpListenerState {
    listener: TcpListener,
    backlog: VecDeque<(i64, String, i64)>,
    accept_waiters: VecDeque<(u64, Arc<TaskOpEntry>)>,
}

struct PendingTcpRead {
    op_id: u64,
    mode: TcpReadMode,
    entry: Arc<TaskOpEntry>,
}

struct PendingTcpWrite {
    op_id: u64,
    data: Vec<u8>,
    offset: usize,
    entry: Arc<TaskOpEntry>,
}

struct TcpStreamState {
    sock: TcpStream,
    eof: bool,
    read_err: Option<(i64, String)>,
    read_buf: VecDeque<u8>,
    pending_read: Option<PendingTcpRead>,
    write_queue: VecDeque<PendingTcpWrite>,
}

struct UdpPacket {
    data: Vec<u8>,
    host: String,
    port: i64,
}

struct PendingUdpSend {
    op_id: u64,
    target: SocketAddr,
    data: Vec<u8>,
    entry: Arc<TaskOpEntry>,
}

struct UdpSocketState {
    sock: UdpSocket,
    recv_err: Option<(i64, String)>,
    recv_queue: VecDeque<UdpPacket>,
    recv_waiters: VecDeque<(u64, usize, Arc<TaskOpEntry>)>,
    send_queue: VecDeque<PendingUdpSend>,
}

#[derive(Clone, Copy)]
enum NetCancelKind {
    TcpAccept(i64),
    TcpConnect,
    TcpRead(i64),
    TcpWrite(i64),
    UdpRecv(i64),
    UdpSend(i64),
}

struct ActiveProcess {
    pid: u32,
    job: Option<Arc<WinJobObject>>,
}

struct WinReactorState {
    doorbell: Arc<Mutex<Doorbell>>,
    waker: Arc<WinWaker>,
    waker_rx: UdpSocket,
    sched: Weak<SchedulerInner>,
    registry: Arc<NetRegistry>,
    timers: FastHashMap<u64, Arc<TaskOpEntry>>,
    timer_heap: BinaryHeap<Reverse<(Instant, u64)>>,
    processes: FastHashMap<u64, ActiveProcess>,
    pool: WinHelperPool,
    listeners: FastHashMap<i64, TcpListenerState>,
    streams: FastHashMap<i64, TcpStreamState>,
    udps: FastHashMap<i64, UdpSocketState>,
    cancel_targets: FastHashMap<u64, NetCancelKind>,
    cancelled_connects: FastHashMap<u64, ()>,
    shutting_down: bool,
}

type StreamConnMeta = (i64, (String, i64), (String, i64));
type IoErrTuple = (i64, String);

impl WinReactorState {
    fn drain_doorbell_and_internal(&mut self) {
        self.waker.notified.store(false, Ordering::Release);
        let mut wake_buf = [0u8; 64];
        while self.waker_rx.recv(&mut wake_buf).is_ok() {}

        let mut internal_batch = VecDeque::new();
        std::mem::swap(&mut internal_batch, &mut *self.waker.internal.lock());
        while let Some(ev) = internal_batch.pop_front() {
            match ev {
                InternalEvent::TcpConnected {
                    op_id,
                    entry,
                    result,
                } => self.finish_tcp_connect(op_id, entry, result),
                InternalEvent::ProcessFinished { op_id } => {
                    self.processes.remove(&op_id);
                }
            }
        }

        let mut cmd_batch = VecDeque::new();
        std::mem::swap(&mut cmd_batch, &mut self.doorbell.lock().queue);
        while let Some(cmd) = cmd_batch.pop_front() {
            match cmd {
                ReactorCmd::StartTimer {
                    op_id,
                    duration_ms,
                    entry,
                } => self.handle_start_timer(op_id, duration_ms, entry),
                ReactorCmd::SpawnProcess { op_id, spec, entry } => {
                    self.handle_spawn_process(op_id, spec, entry)
                }
                ReactorCmd::QueueWork {
                    op_id: _op_id,
                    work,
                    entry,
                } => {
                    self.pool.submit(HelperJob::Work {
                        work,
                        entry,
                        sched: Weak::clone(&self.sched),
                    });
                }
                ReactorCmd::Net(net_cmd) => self.handle_net_cmd(net_cmd),
                ReactorCmd::CancelOp { op_id } => self.handle_cancel_op(op_id),
                ReactorCmd::Shutdown => {
                    self.begin_shutdown();
                    return;
                }
            }
        }
    }

    fn handle_start_timer(&mut self, op_id: u64, duration_ms: u64, entry: Arc<TaskOpEntry>) {
        if duration_ms == 0 {
            complete_entry(&entry, TaskOpResult::Unit, &self.sched);
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(duration_ms);
        self.timers.insert(op_id, entry);
        self.timer_heap.push(Reverse((deadline, op_id)));
    }

    fn fire_expired_timers(&mut self) -> i32 {
        let now = Instant::now();
        while let Some(&Reverse((deadline, op_id))) = self.timer_heap.peek() {
            if deadline > now {
                if !self.timers.contains_key(&op_id) {
                    self.timer_heap.pop();
                    continue;
                }
                let rem_ms = deadline
                    .saturating_duration_since(now)
                    .as_millis()
                    .clamp(1, i32::MAX as u128) as i32;
                return rem_ms;
            }
            self.timer_heap.pop();
            if let Some(entry) = self.timers.remove(&op_id) {
                complete_entry(&entry, TaskOpResult::Unit, &self.sched);
            }
        }
        -1
    }

    fn handle_spawn_process(&mut self, op_id: u64, spec: ProcessSpec, entry: Arc<TaskOpEntry>) {
        if spec.prog.contains('\0')
            || spec.args.iter().any(|a| a.contains('\0'))
            || spec.cwd.as_ref().is_some_and(|c| c.contains('\0'))
        {
            complete_entry(
                &entry,
                TaskOpResult::Process(ProcessOutcome {
                    code: IO_INVALID_INPUT,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: "process arguments must not contain interior NUL bytes".to_string(),
                }),
                &self.sched,
            );
            return;
        }

        let mut cmd = Command::new(&spec.prog);
        cmd.args(&spec.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if spec.stdin_data.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                complete_entry(
                    &entry,
                    TaskOpResult::Process(ProcessOutcome {
                        code: io_error_to_code(&e),
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    }),
                    &self.sched,
                );
                return;
            }
        };

        let pid = child.id();
        let job = WinJobObject::create_for_pid(pid);
        entry.spawned_pid.store(pid as i32, Ordering::Release);

        if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
            if let Some(j) = &job {
                j.terminate();
            }
            let _ = child.kill();
            let _ = child.wait();
            return;
        }

        self.processes.insert(
            op_id,
            ActiveProcess {
                pid,
                job: job.clone(),
            },
        );

        let stdin_data = spec.stdin_data;
        let sched = Weak::clone(&self.sched);
        let waker = Arc::clone(&self.waker);

        let _ = std::thread::Builder::new()
            .name("nessa-proc-wait".to_string())
            .spawn(move || {
                let mut child_stdin = child.stdin.take();
                let mut child_stdout = child.stdout.take();
                let mut child_stderr = child.stderr.take();

                let mut stdout_buf = Vec::new();
                let mut stderr_buf = Vec::new();

                std::thread::scope(|scope| {
                    if let (Some(mut sin), Some(bytes)) = (child_stdin.take(), stdin_data) {
                        scope.spawn(move || {
                            if !bytes.is_empty() {
                                let _ = sin.write_all(&bytes);
                            }
                            drop(sin);
                        });
                    }
                    let err_reader = child_stderr.take().map(|mut serr| {
                        scope.spawn(move || {
                            let mut buf = Vec::new();
                            let _ = serr.read_to_end(&mut buf);
                            buf
                        })
                    });
                    if let Some(mut sout) = child_stdout.take() {
                        let _ = sout.read_to_end(&mut stdout_buf);
                    }
                    if let Some(er) = err_reader
                        && let Ok(b) = er.join()
                    {
                        stderr_buf = b;
                    }
                });

                let exit_code = match child.wait() {
                    Ok(status) => status.code().map_or(-1, i64::from),
                    Err(_) => -1,
                };
                drop(job);

                complete_entry(
                    &entry,
                    TaskOpResult::Process(ProcessOutcome {
                        code: IO_OK,
                        exit_code,
                        stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
                        stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
                    }),
                    &sched,
                );
                waker.push_internal(InternalEvent::ProcessFinished { op_id });
            });
    }

    fn handle_cancel_op(&mut self, op_id: u64) {
        self.timers.remove(&op_id);
        if let Some(proc_info) = self.processes.remove(&op_id) {
            if let Some(job) = &proc_info.job {
                job.terminate();
            }
            terminate_win_pid(proc_info.pid);
        }
        if let Some(kind) = self.cancel_targets.remove(&op_id) {
            match kind {
                NetCancelKind::TcpAccept(listener_id) => {
                    if let Some(lst) = self.listeners.get_mut(&listener_id) {
                        lst.accept_waiters.retain(|(oid, _)| *oid != op_id);
                    }
                }
                NetCancelKind::TcpConnect => {
                    self.cancelled_connects.insert(op_id, ());
                }
                NetCancelKind::TcpRead(stream_id) => {
                    if let Some(st) = self.streams.get_mut(&stream_id)
                        && st.pending_read.as_ref().is_some_and(|pr| pr.op_id == op_id)
                    {
                        st.pending_read = None;
                    }
                }
                NetCancelKind::TcpWrite(stream_id) => {
                    if let Some(st) = self.streams.get_mut(&stream_id) {
                        st.write_queue.retain(|w| w.op_id != op_id || w.offset > 0);
                    }
                }
                NetCancelKind::UdpRecv(sock_id) => {
                    if let Some(u) = self.udps.get_mut(&sock_id) {
                        u.recv_waiters.retain(|(oid, _, _)| *oid != op_id);
                    }
                }
                NetCancelKind::UdpSend(sock_id) => {
                    if let Some(u) = self.udps.get_mut(&sock_id) {
                        u.send_queue.retain(|s| s.op_id != op_id);
                    }
                }
            }
        }
    }

    fn begin_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.timers.clear();
        self.timer_heap.clear();
        for (_, proc_info) in self.processes.drain() {
            if let Some(job) = &proc_info.job {
                job.terminate();
            }
            terminate_win_pid(proc_info.pid);
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
        self.pool.shutdown();
    }

    // -- `std.net` handlers ---------------------------------------------------

    fn handle_net_cmd(&mut self, cmd: NetCmd) {
        if self.shutting_down {
            return;
        }
        match cmd {
            NetCmd::TcpListen {
                op_id: _op_id,
                host,
                port,
                backlog: _backlog,
                entry,
            } => self.cmd_tcp_listen(&host, port, entry),
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
            } => self.cmd_tcp_connect(op_id, host, port, entry),
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
                op_id: _op_id,
                stream_id,
                enable,
                entry,
            } => self.cmd_tcp_set_nodelay(stream_id, enable, entry),
            NetCmd::TcpCloseListener { listener_id } => self.close_listener_by_id(listener_id),
            NetCmd::TcpCloseStream { stream_id } => self.close_stream_by_id(stream_id),
            NetCmd::UdpBind {
                op_id: _op_id,
                host,
                port,
                entry,
            } => self.cmd_udp_bind(&host, port, entry),
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
            NetCmd::ResolveHost {
                op_id: _op_id,
                host,
                entry,
            } => self.cmd_resolve_host(host, entry),
        }
    }

    fn cmd_tcp_listen(&mut self, host: &str, port: i64, entry: Arc<TaskOpEntry>) {
        let addr = match parse_socket_addr(host, port) {
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
        let listener = match TcpListener::bind(addr) {
            Ok(l) => l,
            Err(e) => {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: io_error_to_code(&e),
                        msg: e.to_string(),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            }
        };
        if let Err(e) = listener.set_nonblocking(true) {
            complete_entry(
                &entry,
                TaskOpResult::Net(NetOutcome::HandleAddr {
                    code: io_error_to_code(&e),
                    msg: e.to_string(),
                    handle_id: -1,
                    host: String::new(),
                    port: 0,
                }),
                &self.sched,
            );
            return;
        }
        let (local_ip, local_port) = match listener.local_addr() {
            Ok(sa) => format_sock_addr(sa),
            Err(e) => {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: io_error_to_code(&e),
                        msg: e.to_string(),
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
        self.registry.insert(
            id,
            Arc::new(NetHandleMeta {
                kind: NetHandleKind::TcpListener,
                local_addr: (local_ip.clone(), local_port),
                peer_addr: (String::new(), 0),
                closed: AtomicBool::new(false),
            }),
        );
        self.listeners.insert(
            id,
            TcpListenerState {
                listener,
                backlog: VecDeque::new(),
                accept_waiters: VecDeque::new(),
            },
        );
        complete_entry(
            &entry,
            TaskOpResult::Net(NetOutcome::HandleAddr {
                code: IO_OK,
                msg: String::new(),
                handle_id: id,
                host: local_ip,
                port: local_port,
            }),
            &self.sched,
        );
    }

    fn register_connected_stream(&mut self, sock: TcpStream) -> Result<StreamConnMeta, IoErrTuple> {
        sock.set_nonblocking(true)
            .map_err(|e| (io_error_to_code(&e), e.to_string()))?;
        let local = sock
            .local_addr()
            .map(format_sock_addr)
            .map_err(|e| (io_error_to_code(&e), e.to_string()))?;
        let peer = sock
            .peer_addr()
            .map(format_sock_addr)
            .map_err(|e| (io_error_to_code(&e), e.to_string()))?;
        let id = self.registry.alloc_id();
        self.registry.insert(
            id,
            Arc::new(NetHandleMeta {
                kind: NetHandleKind::TcpStream,
                local_addr: local.clone(),
                peer_addr: peer.clone(),
                closed: AtomicBool::new(false),
            }),
        );
        self.streams.insert(
            id,
            TcpStreamState {
                sock,
                eof: false,
                read_err: None,
                read_buf: VecDeque::new(),
                pending_read: None,
                write_queue: VecDeque::new(),
            },
        );
        Ok((id, local, peer))
    }

    fn cmd_tcp_accept(&mut self, op_id: u64, listener_id: i64, entry: Arc<TaskOpEntry>) {
        if !self.listeners.contains_key(&listener_id) {
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
        if let Some((stream_id, peer_ip, peer_port)) = self
            .listeners
            .get_mut(&listener_id)
            .and_then(|l| l.backlog.pop_front())
        {
            complete_entry(
                &entry,
                TaskOpResult::Net(NetOutcome::HandleAddr {
                    code: IO_OK,
                    msg: String::new(),
                    handle_id: stream_id,
                    host: peer_ip,
                    port: peer_port,
                }),
                &self.sched,
            );
            return;
        }
        let accept_res = self
            .listeners
            .get(&listener_id)
            .expect("verified present")
            .listener
            .accept();
        match accept_res {
            Ok((sock, _)) => match self.register_connected_stream(sock) {
                Ok((stream_id, _, (peer_ip, peer_port))) => {
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: IO_OK,
                            msg: String::new(),
                            handle_id: stream_id,
                            host: peer_ip,
                            port: peer_port,
                        }),
                        &self.sched,
                    );
                }
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
                }
            },
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                if let Some(lst) = self.listeners.get_mut(&listener_id) {
                    lst.accept_waiters.push_back((op_id, entry));
                    self.cancel_targets
                        .insert(op_id, NetCancelKind::TcpAccept(listener_id));
                }
            }
            Err(e) => {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: io_error_to_code(&e),
                        msg: e.to_string(),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
            }
        }
    }

    fn cmd_tcp_connect(&mut self, op_id: u64, host: String, port: i64, entry: Arc<TaskOpEntry>) {
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
        let trimmed = host.trim();
        if trimmed.is_empty() || trimmed.contains('\0') {
            complete_entry(
                &entry,
                TaskOpResult::Net(NetOutcome::HandleAddr {
                    code: IO_INVALID_INPUT,
                    msg: "invalid host".to_string(),
                    handle_id: -1,
                    host: String::new(),
                    port: 0,
                }),
                &self.sched,
            );
            return;
        }
        self.cancel_targets.insert(op_id, NetCancelKind::TcpConnect);
        self.pool.submit(HelperJob::Connect {
            op_id,
            host,
            port: port as u16,
            entry,
            waker: Arc::clone(&self.waker),
        });
    }

    fn finish_tcp_connect(
        &mut self,
        op_id: u64,
        entry: Arc<TaskOpEntry>,
        result: Result<TcpStream, (i64, String)>,
    ) {
        self.cancel_targets.remove(&op_id);
        let was_cancelled = self.cancelled_connects.remove(&op_id).is_some()
            || matches!(*entry.state.lock(), TaskOpState::Cancelled);
        if was_cancelled || self.shutting_down {
            return;
        }
        match result {
            Ok(sock) => match self.register_connected_stream(sock) {
                Ok((stream_id, (local_ip, local_port), _)) => {
                    complete_entry(
                        &entry,
                        TaskOpResult::Net(NetOutcome::HandleAddr {
                            code: IO_OK,
                            msg: String::new(),
                            handle_id: stream_id,
                            host: local_ip,
                            port: local_port,
                        }),
                        &self.sched,
                    );
                }
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
                }
            },
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
            }
        }
    }

    fn cmd_tcp_read(
        &mut self,
        op_id: u64,
        stream_id: i64,
        mode: TcpReadMode,
        entry: Arc<TaskOpEntry>,
    ) {
        let Some(stream) = self.streams.get_mut(&stream_id) else {
            complete_read_closed(&entry, &mode, &self.sched);
            return;
        };
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
        pump_stream_readable(stream);
        if try_satisfy_tcp_read(stream, &mut self.cancel_targets, &self.sched) {
            return;
        }
        self.cancel_targets
            .insert(op_id, NetCancelKind::TcpRead(stream_id));
    }

    fn cmd_tcp_write(
        &mut self,
        op_id: u64,
        stream_id: i64,
        data: Vec<u8>,
        entry: Arc<TaskOpEntry>,
    ) {
        let Some(stream) = self.streams.get_mut(&stream_id) else {
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
        stream.write_queue.push_back(PendingTcpWrite {
            op_id,
            data,
            offset: 0,
            entry,
        });
        self.cancel_targets
            .insert(op_id, NetCancelKind::TcpWrite(stream_id));
        pump_stream_writable(stream, &mut self.cancel_targets, &self.sched);
    }

    fn cmd_tcp_set_nodelay(&mut self, stream_id: i64, enable: bool, entry: Arc<TaskOpEntry>) {
        let Some(stream) = self.streams.get(&stream_id) else {
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
        let (code, msg) = match stream.sock.set_nodelay(enable) {
            Ok(()) => (IO_OK, String::new()),
            Err(e) => (io_error_to_code(&e), e.to_string()),
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

    fn close_listener_by_id(&mut self, listener_id: i64) {
        let Some(mut lst) = self.listeners.remove(&listener_id) else {
            return;
        };
        let _ = self
            .registry
            .mark_closed(listener_id, NetHandleKind::TcpListener);
        for (op_id, waiter) in lst.accept_waiters.drain(..) {
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
        for (queued_stream_id, _, _) in lst.backlog.drain(..) {
            self.close_stream_by_id(queued_stream_id);
        }
    }

    fn close_stream_by_id(&mut self, stream_id: i64) {
        let Some(mut st) = self.streams.remove(&stream_id) else {
            return;
        };
        let _ = self
            .registry
            .mark_closed(stream_id, NetHandleKind::TcpStream);
        if let Some(pr) = st.pending_read.take() {
            self.cancel_targets.remove(&pr.op_id);
            complete_read_closed(&pr.entry, &pr.mode, &self.sched);
        }
        for w in st.write_queue.drain(..) {
            self.cancel_targets.remove(&w.op_id);
            complete_entry(
                &w.entry,
                TaskOpResult::Net(NetOutcome::Count {
                    code: IO_CLOSED,
                    msg: "TCP stream closed".to_string(),
                    count: 0,
                }),
                &self.sched,
            );
        }
    }

    fn cmd_udp_bind(&mut self, host: &str, port: i64, entry: Arc<TaskOpEntry>) {
        let addr = match parse_socket_addr(host, port) {
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
        let sock = match UdpSocket::bind(addr) {
            Ok(s) => s,
            Err(e) => {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: io_error_to_code(&e),
                        msg: e.to_string(),
                        handle_id: -1,
                        host: String::new(),
                        port: 0,
                    }),
                    &self.sched,
                );
                return;
            }
        };
        if let Err(e) = sock.set_nonblocking(true) {
            complete_entry(
                &entry,
                TaskOpResult::Net(NetOutcome::HandleAddr {
                    code: io_error_to_code(&e),
                    msg: e.to_string(),
                    handle_id: -1,
                    host: String::new(),
                    port: 0,
                }),
                &self.sched,
            );
            return;
        }
        let (local_ip, local_port) = match sock.local_addr() {
            Ok(sa) => format_sock_addr(sa),
            Err(e) => {
                complete_entry(
                    &entry,
                    TaskOpResult::Net(NetOutcome::HandleAddr {
                        code: io_error_to_code(&e),
                        msg: e.to_string(),
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
        self.registry.insert(
            id,
            Arc::new(NetHandleMeta {
                kind: NetHandleKind::UdpSocket,
                local_addr: (local_ip.clone(), local_port),
                peer_addr: (String::new(), 0),
                closed: AtomicBool::new(false),
            }),
        );
        self.udps.insert(
            id,
            UdpSocketState {
                sock,
                recv_err: None,
                recv_queue: VecDeque::new(),
                recv_waiters: VecDeque::new(),
                send_queue: VecDeque::new(),
            },
        );
        complete_entry(
            &entry,
            TaskOpResult::Net(NetOutcome::HandleAddr {
                code: IO_OK,
                msg: String::new(),
                handle_id: id,
                host: local_ip,
                port: local_port,
            }),
            &self.sched,
        );
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
        let Some(u) = self.udps.get_mut(&sock_id) else {
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
        let target = match parse_socket_addr(host, port) {
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
        u.send_queue.push_back(PendingUdpSend {
            op_id,
            target,
            data,
            entry,
        });
        self.cancel_targets
            .insert(op_id, NetCancelKind::UdpSend(sock_id));
        pump_udp_writable(u, &mut self.cancel_targets, &self.sched);
    }

    fn cmd_udp_recv(
        &mut self,
        op_id: u64,
        sock_id: i64,
        max_bytes: usize,
        entry: Arc<TaskOpEntry>,
    ) {
        let Some(u) = self.udps.get_mut(&sock_id) else {
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
        u.recv_waiters.push_back((op_id, max_bytes, entry));
        self.cancel_targets
            .insert(op_id, NetCancelKind::UdpRecv(sock_id));
        pump_udp_readable(u, &mut self.cancel_targets, &self.sched);
    }

    fn close_udp_by_id(&mut self, sock_id: i64) {
        let Some(mut u) = self.udps.remove(&sock_id) else {
            return;
        };
        let _ = self.registry.mark_closed(sock_id, NetHandleKind::UdpSocket);
        for (op_id, _, waiter) in u.recv_waiters.drain(..) {
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
        for s in u.send_queue.drain(..) {
            self.cancel_targets.remove(&s.op_id);
            complete_entry(
                &s.entry,
                TaskOpResult::Net(NetOutcome::Count {
                    code: IO_CLOSED,
                    msg: "UDP socket closed".to_string(),
                    count: 0,
                }),
                &self.sched,
            );
        }
    }

    fn cmd_resolve_host(&mut self, host: String, entry: Arc<TaskOpEntry>) {
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
        if trimmed.contains('\0') {
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
        }
        self.pool.submit(HelperJob::Resolve {
            host,
            entry,
            sched: Weak::clone(&self.sched),
        });
    }

    // -- `WSAPoll` event loop cycle -------------------------------------------

    fn poll_once(&mut self, timeout_ms: i32) {
        #[derive(Clone, Copy)]
        enum PollOwner {
            Waker,
            Listener(i64),
            Stream(i64),
            Udp(i64),
        }

        let mut fds: Vec<win_ffi::WsaPollFd> =
            Vec::with_capacity(1 + self.listeners.len() + self.streams.len() + self.udps.len());
        let mut owners: Vec<PollOwner> = Vec::with_capacity(fds.capacity());

        fds.push(win_ffi::WsaPollFd {
            fd: self.waker_rx.as_raw_socket() as usize,
            events: win_ffi::POLLRDNORM,
            revents: 0,
        });
        owners.push(PollOwner::Waker);

        for (&id, lst) in &self.listeners {
            if !lst.accept_waiters.is_empty() {
                fds.push(win_ffi::WsaPollFd {
                    fd: lst.listener.as_raw_socket() as usize,
                    events: win_ffi::POLLRDNORM,
                    revents: 0,
                });
                owners.push(PollOwner::Listener(id));
            }
        }

        for (&id, st) in &self.streams {
            let mut ev: i16 = 0;
            if st.pending_read.is_some()
                && !st.eof
                && st.read_err.is_none()
                && st.read_buf.len() < MAX_BUFFERED_TCP_READ_BYTES
            {
                ev |= win_ffi::POLLRDNORM;
            }
            if !st.write_queue.is_empty() {
                ev |= win_ffi::POLLWRNORM;
            }
            if ev != 0 {
                fds.push(win_ffi::WsaPollFd {
                    fd: st.sock.as_raw_socket() as usize,
                    events: ev,
                    revents: 0,
                });
                owners.push(PollOwner::Stream(id));
            }
        }

        for (&id, u) in &self.udps {
            let mut ev: i16 = 0;
            if !u.recv_waiters.is_empty() && u.recv_queue.len() < 256 {
                ev |= win_ffi::POLLRDNORM;
            }
            if !u.send_queue.is_empty() {
                ev |= win_ffi::POLLWRNORM;
            }
            if ev != 0 {
                fds.push(win_ffi::WsaPollFd {
                    fd: u.sock.as_raw_socket() as usize,
                    events: ev,
                    revents: 0,
                });
                owners.push(PollOwner::Udp(id));
            }
        }

        // SAFETY: `fds` is a valid non-empty array of live Winsock `SOCKET` descriptors.
        let rc = unsafe { win_ffi::WSAPoll(fds.as_mut_ptr(), fds.len() as u32, timeout_ms) };
        if rc <= 0 {
            return;
        }

        let read_mask =
            win_ffi::POLLRDNORM | win_ffi::POLLHUP | win_ffi::POLLERR | win_ffi::POLLNVAL;
        let write_mask =
            win_ffi::POLLWRNORM | win_ffi::POLLHUP | win_ffi::POLLERR | win_ffi::POLLNVAL;

        for (pfd, owner) in fds.into_iter().zip(owners) {
            if pfd.revents == 0 {
                continue;
            }
            match owner {
                PollOwner::Waker => {}
                PollOwner::Listener(id) => {
                    if (pfd.revents & read_mask) != 0 {
                        self.pump_listener_accepts(id);
                    }
                }
                PollOwner::Stream(id) => {
                    if let Some(st) = self.streams.get_mut(&id) {
                        if (pfd.revents & read_mask) != 0 {
                            pump_stream_readable(st);
                            let _ = try_satisfy_tcp_read(st, &mut self.cancel_targets, &self.sched);
                        }
                        if (pfd.revents & write_mask) != 0 {
                            pump_stream_writable(st, &mut self.cancel_targets, &self.sched);
                        }
                    }
                }
                PollOwner::Udp(id) => {
                    if let Some(u) = self.udps.get_mut(&id) {
                        if (pfd.revents & read_mask) != 0 {
                            pump_udp_readable(u, &mut self.cancel_targets, &self.sched);
                        }
                        if (pfd.revents & write_mask) != 0 {
                            pump_udp_writable(u, &mut self.cancel_targets, &self.sched);
                        }
                    }
                }
            }
        }
    }

    fn pump_listener_accepts(&mut self, listener_id: i64) {
        loop {
            let has_waiter = self
                .listeners
                .get(&listener_id)
                .is_some_and(|l| !l.accept_waiters.is_empty());
            if !has_waiter {
                break;
            }
            let accept_res = self
                .listeners
                .get(&listener_id)
                .expect("verified present")
                .listener
                .accept();
            match accept_res {
                Ok((sock, _)) => {
                    let waiter = self
                        .listeners
                        .get_mut(&listener_id)
                        .and_then(|l| l.accept_waiters.pop_front());
                    let Some((op_id, entry)) = waiter else {
                        break;
                    };
                    self.cancel_targets.remove(&op_id);
                    match self.register_connected_stream(sock) {
                        Ok((stream_id, _, (peer_ip, peer_port))) => {
                            complete_entry(
                                &entry,
                                TaskOpResult::Net(NetOutcome::HandleAddr {
                                    code: IO_OK,
                                    msg: String::new(),
                                    handle_id: stream_id,
                                    host: peer_ip,
                                    port: peer_port,
                                }),
                                &self.sched,
                            );
                        }
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
                        }
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    let waiter = self
                        .listeners
                        .get_mut(&listener_id)
                        .and_then(|l| l.accept_waiters.pop_front());
                    if let Some((op_id, entry)) = waiter {
                        self.cancel_targets.remove(&op_id);
                        complete_entry(
                            &entry,
                            TaskOpResult::Net(NetOutcome::HandleAddr {
                                code: io_error_to_code(&e),
                                msg: e.to_string(),
                                handle_id: -1,
                                host: String::new(),
                                port: 0,
                            }),
                            &self.sched,
                        );
                    }
                    break;
                }
            }
        }
    }
}

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

fn pump_stream_readable(stream: &mut TcpStreamState) {
    if stream.eof || stream.read_err.is_some() {
        return;
    }
    let mut buf = [0u8; 16 * 1024];
    while stream.read_buf.len() < MAX_BUFFERED_TCP_READ_BYTES {
        match stream.sock.read(&mut buf) {
            Ok(0) => {
                stream.eof = true;
                break;
            }
            Ok(n) => {
                stream.read_buf.extend(&buf[..n]);
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                stream.read_err = Some((io_error_to_code(&e), e.to_string()));
                break;
            }
        }
    }
}

fn try_satisfy_tcp_read(
    stream: &mut TcpStreamState,
    cancel_targets: &mut FastHashMap<u64, NetCancelKind>,
    sched: &Weak<SchedulerInner>,
) -> bool {
    let Some(pr) = stream.pending_read.as_ref() else {
        return false;
    };
    let outcome = match pr.mode {
        TcpReadMode::Bytes(max_bytes) => {
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
        TcpReadMode::Line => {
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
        let pr = stream.pending_read.take().expect("verified Some");
        cancel_targets.remove(&pr.op_id);
        complete_entry(&pr.entry, TaskOpResult::Net(out), sched);
        true
    } else {
        false
    }
}

fn pump_stream_writable(
    stream: &mut TcpStreamState,
    cancel_targets: &mut FastHashMap<u64, NetCancelKind>,
    sched: &Weak<SchedulerInner>,
) {
    while let Some(front) = stream.write_queue.front_mut() {
        while front.offset < front.data.len() {
            match stream.sock.write(&front.data[front.offset..]) {
                Ok(0) => {
                    let w = stream.write_queue.pop_front().expect("verified front");
                    cancel_targets.remove(&w.op_id);
                    complete_entry(
                        &w.entry,
                        TaskOpResult::Net(NetOutcome::Count {
                            code: IO_BROKEN_PIPE,
                            msg: "TCP write returned 0 bytes".to_string(),
                            count: 0,
                        }),
                        sched,
                    );
                    return;
                }
                Ok(n) => {
                    front.offset += n;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    let w = stream.write_queue.pop_front().expect("verified front");
                    cancel_targets.remove(&w.op_id);
                    complete_entry(
                        &w.entry,
                        TaskOpResult::Net(NetOutcome::Count {
                            code: io_error_to_code(&e),
                            msg: e.to_string(),
                            count: 0,
                        }),
                        sched,
                    );
                    return;
                }
            }
        }
        let w = stream.write_queue.pop_front().expect("verified front");
        cancel_targets.remove(&w.op_id);
        complete_entry(
            &w.entry,
            TaskOpResult::Net(NetOutcome::Count {
                code: IO_OK,
                msg: String::new(),
                count: w.data.len() as i64,
            }),
            sched,
        );
    }
}

fn pump_udp_writable(
    u: &mut UdpSocketState,
    cancel_targets: &mut FastHashMap<u64, NetCancelKind>,
    sched: &Weak<SchedulerInner>,
) {
    while let Some(front) = u.send_queue.front() {
        match u.sock.send_to(&front.data, front.target) {
            Ok(n) => {
                let s = u.send_queue.pop_front().expect("verified front");
                cancel_targets.remove(&s.op_id);
                complete_entry(
                    &s.entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: IO_OK,
                        msg: String::new(),
                        count: n as i64,
                    }),
                    sched,
                );
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                let s = u.send_queue.pop_front().expect("verified front");
                cancel_targets.remove(&s.op_id);
                complete_entry(
                    &s.entry,
                    TaskOpResult::Net(NetOutcome::Count {
                        code: io_error_to_code(&e),
                        msg: e.to_string(),
                        count: 0,
                    }),
                    sched,
                );
            }
        }
    }
}

fn pump_udp_readable(
    u: &mut UdpSocketState,
    cancel_targets: &mut FastHashMap<u64, NetCancelKind>,
    sched: &Weak<SchedulerInner>,
) {
    while !u.recv_waiters.is_empty() {
        if let Some(mut pkt) = u.recv_queue.pop_front() {
            let (op_id, max_bytes, waiter) = u.recv_waiters.pop_front().expect("verified Some");
            cancel_targets.remove(&op_id);
            if pkt.data.len() > max_bytes {
                pkt.data.truncate(max_bytes);
            }
            complete_entry(
                &waiter,
                TaskOpResult::Net(NetOutcome::Datagram {
                    code: IO_OK,
                    msg: String::new(),
                    data: pkt.data,
                    host: pkt.host,
                    port: pkt.port,
                }),
                sched,
            );
            continue;
        }
        if let Some((code, msg)) = u.recv_err.take() {
            let (op_id, _, waiter) = u.recv_waiters.pop_front().expect("verified Some");
            cancel_targets.remove(&op_id);
            complete_entry(
                &waiter,
                TaskOpResult::Net(NetOutcome::Datagram {
                    code,
                    msg,
                    data: Vec::new(),
                    host: String::new(),
                    port: 0,
                }),
                sched,
            );
            continue;
        }
        let mut buf = vec![0u8; 65_536];
        match u.sock.recv_from(&mut buf) {
            Ok((n, peer)) => {
                buf.truncate(n);
                let (peer_ip, peer_port) = format_sock_addr(peer);
                let (op_id, max_bytes, waiter) = u.recv_waiters.pop_front().expect("verified Some");
                cancel_targets.remove(&op_id);
                if buf.len() > max_bytes {
                    buf.truncate(max_bytes);
                }
                complete_entry(
                    &waiter,
                    TaskOpResult::Net(NetOutcome::Datagram {
                        code: IO_OK,
                        msg: String::new(),
                        data: buf,
                        host: peer_ip,
                        port: peer_port,
                    }),
                    sched,
                );
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                let (op_id, _, waiter) = u.recv_waiters.pop_front().expect("verified Some");
                cancel_targets.remove(&op_id);
                complete_entry(
                    &waiter,
                    TaskOpResult::Net(NetOutcome::Datagram {
                        code: io_error_to_code(&e),
                        msg: e.to_string(),
                        data: Vec::new(),
                        host: String::new(),
                        port: 0,
                    }),
                    sched,
                );
            }
        }
    }
}

pub(crate) fn reactor_thread_main(
    doorbell: Arc<Mutex<Doorbell>>,
    waker: Arc<WinWaker>,
    waker_rx: UdpSocket,
    sched: Weak<SchedulerInner>,
    registry: Arc<NetRegistry>,
) {
    let mut state = WinReactorState {
        doorbell,
        waker,
        waker_rx,
        sched,
        registry,
        timers: FastHashMap::default(),
        timer_heap: BinaryHeap::new(),
        processes: FastHashMap::default(),
        pool: WinHelperPool::new(),
        listeners: FastHashMap::default(),
        streams: FastHashMap::default(),
        udps: FastHashMap::default(),
        cancel_targets: FastHashMap::default(),
        cancelled_connects: FastHashMap::default(),
        shutting_down: false,
    };

    while !state.shutting_down {
        state.drain_doorbell_and_internal();
        if state.shutting_down {
            break;
        }
        let timeout_ms = state.fire_expired_timers();
        if state.shutting_down {
            break;
        }
        state.poll_once(timeout_ms);
    }
}
