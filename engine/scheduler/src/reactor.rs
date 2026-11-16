//! Asynchronous `libuv` I/O reactor driving timers, child processes, and threadpool work.
//!
//! A single lazy background thread (`nessa-io-reactor`) owns a dedicated `uv_loop_t` and
//! receives cross-thread commands via a mutex-guarded queue paired with a `uv_async_t`
//! doorbell. Compute workers never block on timers, child processes, or offloaded
//! filesystem operations: tasks register an operation, increment `active_io_handles`,
//! suspend (`TaskStatus::Suspended`), and are woken back into the work-stealing scheduler
//! as soon as the reactor completes or cancels the operation.

use std::any::Any;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::thread::JoinHandle;

use parking_lot::Mutex;
use runtime::{FastHashMap, TaskId};

use crate::SchedulerInner;
use crate::reactor_net::{
    NetCmd, NetHandleKind, NetOutcome, NetRegistry, ReactorNetState, TcpReadMode,
};

// Standardized I/O status codes matching `interpreter::resources::IO_*`.
const IO_OK: i64 = 0;
const IO_NOT_FOUND: i64 = 1;
const IO_PERMISSION_DENIED: i64 = 2;
const IO_ALREADY_EXISTS: i64 = 3;
const IO_INVALID_INPUT: i64 = 4;
const IO_UNEXPECTED_EOF: i64 = 5;
const IO_BROKEN_PIPE: i64 = 7;
const IO_OTHER: i64 = 8;

/// Specification for spawning an asynchronous child process on the reactor.
pub struct ProcessSpec {
    pub prog: String,
    pub args: Vec<String>,
    pub stdin_data: Option<Vec<u8>>,
    pub cwd: Option<String>,
}

/// Completed outcome of an asynchronous child process execution.
#[derive(Debug)]
pub struct ProcessOutcome {
    pub code: i64,
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Completed payload for a task-scoped reactor operation.
pub enum TaskOpResult {
    Unit,
    Process(ProcessOutcome),
    Work(Box<dyn Any + Send>),
    Net(NetOutcome),
}

/// Poll status returned to a retrying builtin on a compute worker.
pub enum TaskOpPoll {
    /// No operation is currently registered for this `TaskId` (submit now).
    Vacant,
    /// Operation is still in flight on the reactor (remain suspended).
    Pending,
    /// Operation completed and its entry has been consumed.
    Ready(TaskOpResult),
}

enum TaskOpState {
    Pending,
    Ready(TaskOpResult),
    Cancelled,
}

pub(crate) struct TaskOpEntry {
    op_id: u64,
    task_id: TaskId,
    io_accounted: AtomicBool,
    spawned_pid: AtomicI32,
    state: Mutex<TaskOpState>,
}

type BoxedWorkFn = Box<dyn FnOnce() -> Box<dyn Any + Send> + Send + 'static>;

enum ReactorCmd {
    StartTimer {
        op_id: u64,
        duration_ms: u64,
        entry: Arc<TaskOpEntry>,
    },
    SpawnProcess {
        op_id: u64,
        spec: ProcessSpec,
        entry: Arc<TaskOpEntry>,
    },
    QueueWork {
        op_id: u64,
        work: BoxedWorkFn,
        entry: Arc<TaskOpEntry>,
    },
    Net(NetCmd),
    CancelOp {
        op_id: u64,
    },
    Shutdown,
}

struct Doorbell {
    running: bool,
    async_handle_addr: usize,
    queue: VecDeque<ReactorCmd>,
}

struct ReactorThread {
    doorbell: Arc<Mutex<Doorbell>>,
    join_handle: Option<JoinHandle<()>>,
}

const OPS_SHARDS: usize = 64;
type OpShardMap = FastHashMap<TaskId, Arc<TaskOpEntry>>;

/// Thread-safe controller managing task-scoped async I/O operations and the lazy reactor thread.
pub struct ReactorController {
    next_op_id: AtomicU64,
    active_ops_count: AtomicUsize,
    ops: Box<[Mutex<OpShardMap>; OPS_SHARDS]>,
    net_registry: Arc<NetRegistry>,
    doorbell_fast: OnceLock<Arc<Mutex<Doorbell>>>,
    thread: Mutex<Option<ReactorThread>>,
}

impl ReactorController {
    pub(crate) fn new() -> Self {
        let shards: [Mutex<OpShardMap>; OPS_SHARDS] =
            std::array::from_fn(|_| Mutex::new(FastHashMap::default()));
        Self {
            next_op_id: AtomicU64::new(1),
            active_ops_count: AtomicUsize::new(0),
            ops: Box::new(shards),
            net_registry: Arc::new(NetRegistry::new()),
            doorbell_fast: OnceLock::new(),
            thread: Mutex::new(None),
        }
    }

    #[inline]
    fn ops_shard(&self, task_id: TaskId) -> &Mutex<FastHashMap<TaskId, Arc<TaskOpEntry>>> {
        &self.ops[(task_id.slot() as usize) & (OPS_SHARDS - 1)]
    }

    /// Poll whether `task_id` has an in-flight or completed async operation.
    pub fn poll_task_op(&self, task_id: TaskId) -> TaskOpPoll {
        let mut ops = self.ops_shard(task_id).lock();
        let Some(entry) = ops.get(&task_id) else {
            return TaskOpPoll::Vacant;
        };
        let mut st = entry.state.lock();
        match std::mem::replace(&mut *st, TaskOpState::Pending) {
            TaskOpState::Pending => TaskOpPoll::Pending,
            TaskOpState::Ready(res) => {
                drop(st);
                if ops.remove(&task_id).is_some() {
                    self.active_ops_count.fetch_sub(1, Ordering::AcqRel);
                }
                TaskOpPoll::Ready(res)
            }
            TaskOpState::Cancelled => {
                drop(st);
                if ops.remove(&task_id).is_some() {
                    self.active_ops_count.fetch_sub(1, Ordering::AcqRel);
                }
                TaskOpPoll::Vacant
            }
        }
    }

    fn ensure_thread<'a>(&'a self, sched: &Arc<SchedulerInner>) -> &'a Mutex<Doorbell> {
        if let Some(db) = self.doorbell_fast.get() {
            return db;
        }
        let mut guard = self.thread.lock();
        if let Some(existing) = guard.as_ref() {
            let _ = self.doorbell_fast.set(Arc::clone(&existing.doorbell));
            return self.doorbell_fast.get().expect("doorbell initialized");
        }

        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<usize>(1);
        let doorbell = Arc::new(Mutex::new(Doorbell {
            running: true,
            async_handle_addr: 0,
            queue: VecDeque::new(),
        }));
        let doorbell_for_thread = Arc::clone(&doorbell);
        let weak_sched = Arc::downgrade(sched);
        let net_reg = Arc::clone(&self.net_registry);

        let join_handle = std::thread::Builder::new()
            .name("nessa-io-reactor".to_string())
            .spawn(move || {
                reactor_thread_main(doorbell_for_thread, weak_sched, net_reg, init_tx);
            })
            .expect("failed to spawn nessa-io-reactor thread");

        let async_addr = init_rx
            .recv()
            .expect("nessa-io-reactor failed during initialization");
        {
            let mut db = doorbell.lock();
            db.async_handle_addr = async_addr;
            if !db.queue.is_empty() && db.running {
                // SAFETY: `async_addr` is a live `uv_async_t` owned by the running reactor loop.
                unsafe {
                    let _ = libuv_sys2::uv_async_send(async_addr as *mut libuv_sys2::uv_async_t);
                }
            }
        }

        let _ = self.doorbell_fast.set(Arc::clone(&doorbell));
        *guard = Some(ReactorThread {
            doorbell,
            join_handle: Some(join_handle),
        });
        self.doorbell_fast.get().expect("doorbell initialized")
    }

    fn push_cmd(&self, sched: &Arc<SchedulerInner>, cmd: ReactorCmd) {
        let doorbell = self.ensure_thread(sched);
        let mut db = doorbell.lock();
        if db.running {
            db.queue.push_back(cmd);
            if db.async_handle_addr != 0 {
                // SAFETY: While `db.running` is true and `db` is locked, `async_handle_addr`
                // points to an initialized, non-closing `uv_async_t` on the reactor loop.
                unsafe {
                    let _ = libuv_sys2::uv_async_send(
                        db.async_handle_addr as *mut libuv_sys2::uv_async_t,
                    );
                }
            }
        }
    }

    fn push_cmd_if_started(&self, cmd: ReactorCmd) {
        if let Some(doorbell) = self.doorbell_fast.get() {
            let mut db = doorbell.lock();
            if db.running {
                db.queue.push_back(cmd);
                if db.async_handle_addr != 0 {
                    // SAFETY: Protected by `db.running` under `doorbell` mutex.
                    unsafe {
                        let _ = libuv_sys2::uv_async_send(
                            db.async_handle_addr as *mut libuv_sys2::uv_async_t,
                        );
                    }
                }
            }
        }
    }

    fn register_entry(&self, sched: &SchedulerInner, task_id: TaskId) -> (u64, Arc<TaskOpEntry>) {
        let op_id = self.next_op_id.fetch_add(1, Ordering::Relaxed);
        let entry = Arc::new(TaskOpEntry {
            op_id,
            task_id,
            io_accounted: AtomicBool::new(true),
            spawned_pid: AtomicI32::new(0),
            state: Mutex::new(TaskOpState::Pending),
        });
        sched.inc_active_io();
        self.active_ops_count.fetch_add(1, Ordering::AcqRel);
        let prev = self
            .ops_shard(task_id)
            .lock()
            .insert(task_id, Arc::clone(&entry));
        if let Some(stale) = prev {
            self.active_ops_count.fetch_sub(1, Ordering::AcqRel);
            *stale.state.lock() = TaskOpState::Cancelled;
            if stale.io_accounted.swap(false, Ordering::AcqRel) {
                sched.dec_active_io();
            }
            self.push_cmd_if_started(ReactorCmd::CancelOp { op_id: stale.op_id });
        }
        (op_id, entry)
    }

    /// Submit a non-blocking timer (`uv_timer_t`) for `task_id`.
    pub(crate) fn submit_sleep(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        duration_ms: u64,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::StartTimer {
                op_id,
                duration_ms,
                entry,
            },
        );
    }

    /// Submit a non-blocking child process (`uv_spawn` + `uv_pipe_t`) for `task_id`.
    pub(crate) fn submit_process(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        spec: ProcessSpec,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(sched, ReactorCmd::SpawnProcess { op_id, spec, entry });
    }

    /// Offload a blocking closure onto `libuv`'s `uv_queue_work` threadpool for `task_id`.
    pub(crate) fn submit_work<F, T>(&self, sched: &Arc<SchedulerInner>, task_id: TaskId, work: F)
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (op_id, entry) = self.register_entry(sched, task_id);
        let boxed: BoxedWorkFn = Box::new(move || Box::new(work()));
        self.push_cmd(
            sched,
            ReactorCmd::QueueWork {
                op_id,
                work: boxed,
                entry,
            },
        );
    }

    /// Cancel any pending or unread operation belonging to `task_id`.
    ///
    /// Safe to call from any thread (including during subtree cancellation or task reaping).
    pub(crate) fn cancel_task(&self, sched: &SchedulerInner, task_id: TaskId) {
        if self.active_ops_count.load(Ordering::Acquire) == 0 {
            return;
        }
        let removed = self.ops_shard(task_id).lock().remove(&task_id);
        let Some(entry) = removed else {
            return;
        };
        self.active_ops_count.fetch_sub(1, Ordering::AcqRel);
        *entry.state.lock() = TaskOpState::Cancelled;
        let pid = entry.spawned_pid.load(Ordering::Acquire);
        if pid > 0 {
            // SAFETY: `uv_kill` is a thin wrapper around `kill(2)` / Win32 `TerminateProcess`.
            // On Unix, child processes are spawned with `UV_PROCESS_DETACHED` so `-pid`
            // terminates the entire child process group (shell + grandchildren).
            unsafe {
                #[cfg(unix)]
                {
                    let _ = libuv_sys2::uv_kill(-pid, libuv_sys2::SIGKILL as c_int);
                }
                let _ = libuv_sys2::uv_kill(pid, libuv_sys2::SIGKILL as c_int);
            }
        }
        if entry.io_accounted.swap(false, Ordering::AcqRel) {
            let _ = sched.wake_from_reactor(task_id);
            sched.dec_active_io();
        }
        self.push_cmd_if_started(ReactorCmd::CancelOp { op_id: entry.op_id });
    }

    pub(crate) fn shutdown(&self) {
        let taken = self.thread.lock().take();
        if let Some(mut rt) = taken {
            {
                let mut db = rt.doorbell.lock();
                if db.running {
                    db.queue.push_back(ReactorCmd::Shutdown);
                    db.running = false;
                    if db.async_handle_addr != 0 {
                        // SAFETY: Final wake of `uv_async_t` before joining `join_handle`.
                        unsafe {
                            let _ = libuv_sys2::uv_async_send(
                                db.async_handle_addr as *mut libuv_sys2::uv_async_t,
                            );
                        }
                    }
                }
            }
            if let Some(jh) = rt.join_handle.take() {
                let _ = jh.join();
            }
        }
        for shard in self.ops.iter() {
            shard.lock().clear();
        }
        self.net_registry.clear();
        self.active_ops_count.store(0, Ordering::Release);
    }

    // -- `std.net` controller operations --------------------------------------

    pub(crate) fn submit_tcp_listen(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        host: String,
        port: i64,
        backlog: i64,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::TcpListen {
                op_id,
                host,
                port,
                backlog,
                entry,
            }),
        );
    }

    pub(crate) fn submit_tcp_accept(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        listener_id: i64,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::TcpAccept {
                op_id,
                listener_id,
                entry,
            }),
        );
    }

    pub(crate) fn submit_tcp_connect(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        host: String,
        port: i64,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::TcpConnect {
                op_id,
                host,
                port,
                entry,
            }),
        );
    }

    pub(crate) fn submit_tcp_read_bytes(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        stream_id: i64,
        max_bytes: usize,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::TcpRead {
                op_id,
                stream_id,
                mode: TcpReadMode::Bytes(max_bytes),
                entry,
            }),
        );
    }

    pub(crate) fn submit_tcp_read_line(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        stream_id: i64,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::TcpRead {
                op_id,
                stream_id,
                mode: TcpReadMode::Line,
                entry,
            }),
        );
    }

    pub(crate) fn submit_tcp_write(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        stream_id: i64,
        data: Vec<u8>,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::TcpWrite {
                op_id,
                stream_id,
                data,
                entry,
            }),
        );
    }

    pub(crate) fn submit_tcp_set_nodelay(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        stream_id: i64,
        enable: bool,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::TcpSetNodelay {
                op_id,
                stream_id,
                enable,
                entry,
            }),
        );
    }

    pub(crate) fn tcp_local_addr(&self, id: i64) -> Option<(String, i64)> {
        if let Some(m) = self.net_registry.get_open(id, NetHandleKind::TcpListener) {
            return Some(m.local_addr.clone());
        }
        self.net_registry
            .get_open(id, NetHandleKind::TcpStream)
            .map(|m| m.local_addr.clone())
    }

    pub(crate) fn tcp_peer_addr(&self, id: i64) -> Option<(String, i64)> {
        self.net_registry
            .get_open(id, NetHandleKind::TcpStream)
            .map(|m| m.peer_addr.clone())
    }

    pub(crate) fn tcp_close(&self, id: i64) -> bool {
        if let Ok(newly_closed) = self
            .net_registry
            .mark_closed(id, NetHandleKind::TcpListener)
        {
            if newly_closed {
                self.push_cmd_if_started(ReactorCmd::Net(NetCmd::TcpCloseListener {
                    listener_id: id,
                }));
            }
            return true;
        }
        if let Ok(newly_closed) = self.net_registry.mark_closed(id, NetHandleKind::TcpStream) {
            if newly_closed {
                self.push_cmd_if_started(ReactorCmd::Net(NetCmd::TcpCloseStream { stream_id: id }));
            }
            return true;
        }
        false
    }

    pub(crate) fn submit_udp_bind(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        host: String,
        port: i64,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::UdpBind {
                op_id,
                host,
                port,
                entry,
            }),
        );
    }

    pub(crate) fn submit_udp_send(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        sock_id: i64,
        host: String,
        port: i64,
        data: Vec<u8>,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::UdpSend {
                op_id,
                sock_id,
                host,
                port,
                data,
                entry,
            }),
        );
    }

    pub(crate) fn submit_udp_recv(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        sock_id: i64,
        max_bytes: usize,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::UdpRecv {
                op_id,
                sock_id,
                max_bytes,
                entry,
            }),
        );
    }

    pub(crate) fn udp_local_addr(&self, id: i64) -> Option<(String, i64)> {
        self.net_registry
            .get_open(id, NetHandleKind::UdpSocket)
            .map(|m| m.local_addr.clone())
    }

    pub(crate) fn udp_close(&self, id: i64) -> bool {
        if let Ok(newly_closed) = self.net_registry.mark_closed(id, NetHandleKind::UdpSocket) {
            if newly_closed {
                self.push_cmd_if_started(ReactorCmd::Net(NetCmd::UdpClose { sock_id: id }));
            }
            return true;
        }
        false
    }

    pub(crate) fn submit_resolve_host(
        &self,
        sched: &Arc<SchedulerInner>,
        task_id: TaskId,
        host: String,
    ) {
        let (op_id, entry) = self.register_entry(sched, task_id);
        self.push_cmd(
            sched,
            ReactorCmd::Net(NetCmd::ResolveHost { op_id, host, entry }),
        );
    }
}

// ---------------------------------------------------------------------------
// Aligned opaque libuv allocation helpers
// ---------------------------------------------------------------------------

/// Allocate a zero-initialized, 8-byte-aligned buffer of `byte_size` bytes for an opaque
/// `libuv` struct, storing the slice word length in a 1-word prefix right before the returned
/// pointer so [`free_uv_aligned`] never relies on `uv_handle_get_type`.
pub(crate) unsafe fn alloc_uv_aligned<T>(byte_size: usize) -> *mut T {
    assert!(byte_size > 0, "libuv struct size must be non-zero");
    let payload_words = byte_size.div_ceil(8);
    let total_words = payload_words + 1;
    let mut buf = vec![0u64; total_words];
    buf[0] = total_words as u64;
    let base = Box::into_raw(buf.into_boxed_slice()) as *mut u64;
    // SAFETY: `base` owns `total_words >= 2` aligned `u64` words; `base.add(1)` is 8-byte aligned.
    unsafe { base.add(1) as *mut T }
}

/// Free a buffer previously allocated by [`alloc_uv_aligned`].
pub(crate) unsafe fn free_uv_aligned<T>(ptr: *mut T) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` was returned by `alloc_uv_aligned` with `total_words` stored at `ptr - 8`,
    // and libuv has finished all callbacks referencing `ptr`.
    unsafe {
        let base = (ptr as *mut u64).sub(1);
        let total_words = (*base) as usize;
        let slice = ptr::slice_from_raw_parts_mut(base, total_words);
        drop(Box::from_raw(slice));
    }
}

pub(crate) fn uv_error_to_io_code(uv_err: c_int) -> i64 {
    match uv_err {
        x if x == libuv_sys2::uv_errno_t_UV_ENOENT => IO_NOT_FOUND,
        x if x == libuv_sys2::uv_errno_t_UV_EACCES || x == libuv_sys2::uv_errno_t_UV_EPERM => {
            IO_PERMISSION_DENIED
        }
        x if x == libuv_sys2::uv_errno_t_UV_EEXIST || x == libuv_sys2::uv_errno_t_UV_EADDRINUSE => {
            IO_ALREADY_EXISTS
        }
        x if x == libuv_sys2::uv_errno_t_UV_EINVAL
            || x == libuv_sys2::uv_errno_t_UV_EAFNOSUPPORT =>
        {
            IO_INVALID_INPUT
        }
        x if x == libuv_sys2::uv_errno_t_UV_EOF => IO_UNEXPECTED_EOF,
        x if x == libuv_sys2::uv_errno_t_UV_EPIPE || x == libuv_sys2::uv_errno_t_UV_ECONNRESET => {
            IO_BROKEN_PIPE
        }
        _ => IO_OTHER,
    }
}

pub(crate) fn uv_error_message(uv_err: c_int) -> String {
    let mut buf = [0 as c_char; 256];
    // SAFETY: `uv_strerror_r` formats thread-safely into stack buffer `buf`.
    unsafe {
        let ptr = libuv_sys2::uv_strerror_r(uv_err, buf.as_mut_ptr(), buf.len());
        if ptr.is_null() {
            format!("libuv error {uv_err}")
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

pub(crate) fn complete_entry(
    entry: &Arc<TaskOpEntry>,
    result: TaskOpResult,
    sched: &Weak<SchedulerInner>,
) {
    {
        let mut st = entry.state.lock();
        if matches!(*st, TaskOpState::Cancelled) {
            return;
        }
        *st = TaskOpState::Ready(result);
    }
    if entry.io_accounted.swap(false, Ordering::AcqRel)
        && let Some(s) = sched.upgrade()
    {
        let _ = s.wake_from_reactor(entry.task_id);
        s.dec_active_io();
    }
}

// ---------------------------------------------------------------------------
// Reactor loop state & callbacks (exclusively accessed on `nessa-io-reactor`)
// ---------------------------------------------------------------------------

struct ReactorLoopState {
    loop_ptr: *mut libuv_sys2::uv_loop_t,
    async_ptr: *mut libuv_sys2::uv_async_t,
    doorbell: Arc<Mutex<Doorbell>>,
    sched: Weak<SchedulerInner>,
    timers: FastHashMap<u64, *mut TimerCtx>,
    free_timers: Vec<*mut TimerCtx>,
    processes: FastHashMap<u64, *mut ProcessCtx>,
    works: FastHashMap<u64, *mut libuv_sys2::uv_work_t>,
    net: Box<ReactorNetState>,
    shutting_down: bool,
}

struct TimerCtx {
    op_id: u64,
    handle: *mut libuv_sys2::uv_timer_t,
    loop_state: *mut ReactorLoopState,
    entry: Option<Arc<TaskOpEntry>>,
    closing: bool,
}

#[derive(Clone, Copy)]
enum PipeRole {
    Stdout,
    Stderr,
    Stdin,
}

struct PipeSlot {
    role: PipeRole,
    handle: *mut libuv_sys2::uv_pipe_t,
    proc_ctx: *mut ProcessCtx,
    closing: bool,
}

struct StdinWriteCtx {
    req: *mut libuv_sys2::uv_write_t,
    _data: Vec<u8>,
    proc_ctx: *mut ProcessCtx,
}

struct ProcessCtx {
    op_id: u64,
    proc_handle: *mut libuv_sys2::uv_process_t,
    proc_closing: bool,
    stdout_pipe: *mut PipeSlot,
    stderr_pipe: *mut PipeSlot,
    stdin_pipe: *mut PipeSlot,
    stdout_done: bool,
    stderr_done: bool,
    exited: bool,
    exit_status: i64,
    term_signal: c_int,
    stdout_buf: Vec<u8>,
    stderr_buf: Vec<u8>,
    open_handles: usize,
    completed: bool,
    loop_state: *mut ReactorLoopState,
    entry: Arc<TaskOpEntry>,
}

struct WorkCtx {
    op_id: u64,
    req: *mut libuv_sys2::uv_work_t,
    loop_state: *mut ReactorLoopState,
    entry: Arc<TaskOpEntry>,
    work: Option<BoxedWorkFn>,
    output: Option<Box<dyn Any + Send>>,
}

unsafe extern "C" fn on_async_doorbell(handle: *mut libuv_sys2::uv_async_t) {
    // SAFETY: `handle` data was set to `*mut ReactorLoopState` on `nessa-io-reactor`.
    let state = unsafe {
        &mut *(libuv_sys2::uv_handle_get_data(handle as *mut libuv_sys2::uv_handle_t)
            as *mut ReactorLoopState)
    };
    if state.shutting_down {
        return;
    }
    let mut batch = VecDeque::new();
    {
        let mut db = state.doorbell.lock();
        std::mem::swap(&mut batch, &mut db.queue);
    }
    while let Some(cmd) = batch.pop_front() {
        match cmd {
            ReactorCmd::StartTimer {
                op_id,
                duration_ms,
                entry,
            } => state.handle_start_timer(op_id, duration_ms, entry),
            ReactorCmd::SpawnProcess { op_id, spec, entry } => {
                state.handle_spawn_process(op_id, spec, entry)
            }
            ReactorCmd::QueueWork { op_id, work, entry } => {
                state.handle_queue_work(op_id, work, entry)
            }
            ReactorCmd::Net(net_cmd) => state.net.handle_cmd(net_cmd),
            ReactorCmd::CancelOp { op_id } => state.handle_cancel_op(op_id),
            ReactorCmd::Shutdown => {
                state.begin_shutdown();
                return;
            }
        }
    }
}

unsafe extern "C" fn on_async_close(handle: *mut libuv_sys2::uv_handle_t) {
    // SAFETY: `uv_close` finished closing the doorbell `uv_async_t`.
    unsafe { free_uv_aligned(handle) };
}

// -- Timer callbacks ---------------------------------------------------------

const MAX_POOLED_TIMERS: usize = 16_384;

unsafe extern "C" fn on_timer_fire(handle: *mut libuv_sys2::uv_timer_t) {
    // SAFETY: `handle` data points to a live `TimerCtx` on the reactor thread.
    let ctx_ptr = unsafe {
        libuv_sys2::uv_handle_get_data(handle as *mut libuv_sys2::uv_handle_t) as *mut TimerCtx
    };
    if ctx_ptr.is_null() {
        return;
    }
    // SAFETY: Exclusively accessed on the reactor thread.
    let ctx = unsafe { &mut *ctx_ptr };
    // SAFETY: `ctx.loop_state` outlives all handles on the loop.
    let state = unsafe { &mut *ctx.loop_state };
    state.timers.remove(&ctx.op_id);
    if let Some(entry) = ctx.entry.take() {
        complete_entry(&entry, TaskOpResult::Unit, &state.sched);
    }
    recycle_or_close_timer_ctx(state, ctx_ptr);
}

fn recycle_or_close_timer_ctx(state: &mut ReactorLoopState, ctx_ptr: *mut TimerCtx) {
    // SAFETY: Called on the reactor thread for an active `TimerCtx`.
    unsafe {
        let ctx = &mut *ctx_ptr;
        ctx.entry = None;
        if !state.shutting_down && !ctx.closing && state.free_timers.len() < MAX_POOLED_TIMERS {
            let _ = libuv_sys2::uv_timer_stop(ctx.handle);
            state.free_timers.push(ctx_ptr);
        } else {
            close_timer_ctx(ctx_ptr);
        }
    }
}

unsafe extern "C" fn on_timer_close(handle: *mut libuv_sys2::uv_handle_t) {
    // SAFETY: `handle` was initialized with `*mut TimerCtx` and is now closed.
    unsafe {
        let ctx_ptr = libuv_sys2::uv_handle_get_data(handle) as *mut TimerCtx;
        free_uv_aligned(handle);
        if !ctx_ptr.is_null() {
            drop(Box::from_raw(ctx_ptr));
        }
    }
}

fn close_timer_ctx(ctx_ptr: *mut TimerCtx) {
    // SAFETY: Called on the reactor thread for an active `TimerCtx`.
    unsafe {
        let ctx = &mut *ctx_ptr;
        if ctx.closing {
            return;
        }
        ctx.closing = true;
        let _ = libuv_sys2::uv_timer_stop(ctx.handle);
        libuv_sys2::uv_close(
            ctx.handle as *mut libuv_sys2::uv_handle_t,
            Some(on_timer_close),
        );
    }
}

// -- Child process & pipe callbacks ------------------------------------------

unsafe extern "C" fn on_pipe_alloc(
    _handle: *mut libuv_sys2::uv_handle_t,
    suggested_size: usize,
    buf: *mut libuv_sys2::uv_buf_t,
) {
    let cap = suggested_size.clamp(1024, 65_536);
    let mut vec = vec![0u8; cap];
    let ptr = vec.as_mut_ptr() as *mut c_char;
    let len = vec.capacity();
    std::mem::forget(vec);
    // SAFETY: `buf` is a valid out-pointer provided by libuv right before `on_pipe_read`.
    unsafe {
        (*buf).base = ptr;
        (*buf).len = len;
    }
}

unsafe extern "C" fn on_pipe_read(
    stream: *mut libuv_sys2::uv_stream_t,
    nread: isize,
    buf: *const libuv_sys2::uv_buf_t,
) {
    // SAFETY: `buf` is either null/empty or owns the exact `Vec<u8>` buffer allocated in `on_pipe_alloc`.
    let slice_data = unsafe {
        if !buf.is_null() && !(*buf).base.is_null() && (*buf).len > 0 {
            let len = (*buf).len;
            let raw = Vec::from_raw_parts((*buf).base as *mut u8, len, len);
            Some(raw)
        } else {
            None
        }
    };

    // SAFETY: `stream` data points to `PipeSlot` on the reactor thread.
    let slot_ptr = unsafe {
        libuv_sys2::uv_handle_get_data(stream as *mut libuv_sys2::uv_handle_t) as *mut PipeSlot
    };
    if slot_ptr.is_null() {
        return;
    }
    // SAFETY: Reactor thread owns `PipeSlot` and `ProcessCtx`.
    unsafe {
        let slot = &mut *slot_ptr;
        let pctx = &mut *slot.proc_ctx;
        if nread > 0 {
            if let Some(raw) = &slice_data {
                let n = (nread as usize).min(raw.len());
                match slot.role {
                    PipeRole::Stdout => pctx.stdout_buf.extend_from_slice(&raw[..n]),
                    PipeRole::Stderr => pctx.stderr_buf.extend_from_slice(&raw[..n]),
                    PipeRole::Stdin => {}
                }
            }
        } else if nread < 0 {
            match slot.role {
                PipeRole::Stdout => pctx.stdout_done = true,
                PipeRole::Stderr => pctx.stderr_done = true,
                PipeRole::Stdin => {}
            }
            close_pipe_slot(slot_ptr);
            maybe_finish_process(slot.proc_ctx);
        }
    }
}

unsafe extern "C" fn on_stdin_write(req: *mut libuv_sys2::uv_write_t, _status: c_int) {
    // SAFETY: `req` data points to `Box<StdinWriteCtx>` holding an `open_handles` ref on `proc_ctx`.
    unsafe {
        let wctx_ptr =
            libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut StdinWriteCtx;
        if !wctx_ptr.is_null() {
            let wctx = Box::from_raw(wctx_ptr);
            close_pipe_slot((*wctx.proc_ctx).stdin_pipe);
            free_uv_aligned(wctx.req);
            decrement_process_open_handles(wctx.proc_ctx);
        }
    }
}

unsafe extern "C" fn on_proc_exit(
    proc_handle: *mut libuv_sys2::uv_process_t,
    exit_status: i64,
    term_signal: c_int,
) {
    // SAFETY: `proc_handle` data points to `ProcessCtx` on the reactor thread.
    let pctx_ptr = unsafe {
        libuv_sys2::uv_handle_get_data(proc_handle as *mut libuv_sys2::uv_handle_t)
            as *mut ProcessCtx
    };
    if pctx_ptr.is_null() {
        return;
    }
    // SAFETY: Reactor thread owns `ProcessCtx`.
    unsafe {
        let pctx = &mut *pctx_ptr;
        pctx.exited = true;
        pctx.exit_status = exit_status;
        pctx.term_signal = term_signal;
        close_proc_handle(pctx_ptr);
        maybe_finish_process(pctx_ptr);
    }
}

unsafe extern "C" fn on_pipe_close(handle: *mut libuv_sys2::uv_handle_t) {
    // SAFETY: `handle` is a closed `uv_pipe_t` whose data is `*mut PipeSlot`.
    unsafe {
        let slot_ptr = libuv_sys2::uv_handle_get_data(handle) as *mut PipeSlot;
        free_uv_aligned(handle);
        if !slot_ptr.is_null() {
            let slot = Box::from_raw(slot_ptr);
            let proc_ctx = slot.proc_ctx;
            let pctx = &mut *proc_ctx;
            match slot.role {
                PipeRole::Stdin => pctx.stdin_pipe = ptr::null_mut(),
                PipeRole::Stdout => pctx.stdout_pipe = ptr::null_mut(),
                PipeRole::Stderr => pctx.stderr_pipe = ptr::null_mut(),
            }
            drop(slot);
            decrement_process_open_handles(proc_ctx);
        }
    }
}

unsafe extern "C" fn on_proc_close(handle: *mut libuv_sys2::uv_handle_t) {
    // SAFETY: `handle` is a closed `uv_process_t` whose data is `*mut ProcessCtx`.
    unsafe {
        let pctx_ptr = libuv_sys2::uv_handle_get_data(handle) as *mut ProcessCtx;
        free_uv_aligned(handle);
        if !pctx_ptr.is_null() {
            decrement_process_open_handles(pctx_ptr);
        }
    }
}

unsafe fn close_pipe_slot(slot_ptr: *mut PipeSlot) {
    if slot_ptr.is_null() {
        return;
    }
    // SAFETY: Reactor thread owns `slot_ptr`.
    unsafe {
        let slot = &mut *slot_ptr;
        if slot.closing {
            return;
        }
        slot.closing = true;
        let _ = libuv_sys2::uv_read_stop(slot.handle as *mut libuv_sys2::uv_stream_t);
        libuv_sys2::uv_close(
            slot.handle as *mut libuv_sys2::uv_handle_t,
            Some(on_pipe_close),
        );
    }
}

unsafe fn close_proc_handle(pctx_ptr: *mut ProcessCtx) {
    // SAFETY: Reactor thread owns `pctx_ptr`.
    unsafe {
        let pctx = &mut *pctx_ptr;
        if pctx.proc_closing {
            return;
        }
        pctx.proc_closing = true;
        let h = pctx.proc_handle as *mut libuv_sys2::uv_handle_t;
        if libuv_sys2::uv_handle_get_type(h) == libuv_sys2::uv_handle_type_UV_PROCESS {
            libuv_sys2::uv_close(h, Some(on_proc_close));
        } else {
            free_uv_aligned(pctx.proc_handle);
            decrement_process_open_handles(pctx_ptr);
        }
    }
}

unsafe fn maybe_finish_process(pctx_ptr: *mut ProcessCtx) {
    // SAFETY: Reactor thread owns `pctx_ptr`.
    unsafe {
        let pctx = &mut *pctx_ptr;
        if pctx.completed || !pctx.exited || !pctx.stdout_done || !pctx.stderr_done {
            return;
        }
        pctx.completed = true;
        let state = &mut *pctx.loop_state;
        state.processes.remove(&pctx.op_id);

        let exit_code = if pctx.term_signal != 0 {
            128 + i64::from(pctx.term_signal)
        } else {
            pctx.exit_status
        };
        let stdout = String::from_utf8_lossy(&pctx.stdout_buf).into_owned();
        let stderr = String::from_utf8_lossy(&pctx.stderr_buf).into_owned();
        complete_entry(
            &pctx.entry,
            TaskOpResult::Process(ProcessOutcome {
                code: IO_OK,
                exit_code,
                stdout,
                stderr,
            }),
            &state.sched,
        );
    }
}

unsafe fn cancel_process_ctx(pctx_ptr: *mut ProcessCtx) {
    // SAFETY: Reactor thread owns `pctx_ptr`. Leave `proc_handle` open until `on_proc_exit`
    // fires so `libuv`'s `SIGCHLD` handler reaps the killed child via `waitpid` without leaving
    // a zombie process in the OS table.
    unsafe {
        let pctx = &mut *pctx_ptr;
        pctx.completed = true;
        let pid = pctx.entry.spawned_pid.load(Ordering::Acquire);
        if pid > 0 {
            #[cfg(unix)]
            {
                let _ = libuv_sys2::uv_kill(-pid, libuv_sys2::SIGKILL as c_int);
            }
            let _ = libuv_sys2::uv_kill(pid, libuv_sys2::SIGKILL as c_int);
        }
        if !pctx.proc_closing && !pctx.exited {
            let _ = libuv_sys2::uv_process_kill(pctx.proc_handle, libuv_sys2::SIGKILL as c_int);
        }
        close_pipe_slot(pctx.stdin_pipe);
        close_pipe_slot(pctx.stdout_pipe);
        close_pipe_slot(pctx.stderr_pipe);
        if pctx.exited {
            close_proc_handle(pctx_ptr);
        }
    }
}

unsafe fn decrement_process_open_handles(pctx_ptr: *mut ProcessCtx) {
    // SAFETY: Reactor thread owns `pctx_ptr`; freed once all associated handles finish `uv_close`.
    unsafe {
        let pctx = &mut *pctx_ptr;
        pctx.open_handles = pctx.open_handles.saturating_sub(1);
        if pctx.open_handles == 0 {
            let state = &mut *pctx.loop_state;
            state.processes.remove(&pctx.op_id);
            drop(Box::from_raw(pctx_ptr));
        }
    }
}

// -- Threadpool `uv_queue_work` callbacks ------------------------------------

unsafe extern "C" fn on_work_exec(req: *mut libuv_sys2::uv_work_t) {
    // SAFETY: `req` data points to `WorkCtx`; while `on_work_exec` runs on a libuv worker
    // thread, `after_work_cb` has not started yet so `work` and `output` are exclusively ours.
    let wctx = unsafe {
        &mut *(libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut WorkCtx)
    };
    if matches!(*wctx.entry.state.lock(), TaskOpState::Cancelled) {
        return;
    }
    if let Some(work_fn) = wctx.work.take() {
        wctx.output = Some(work_fn());
    }
}

unsafe extern "C" fn on_work_after(req: *mut libuv_sys2::uv_work_t, status: c_int) {
    // SAFETY: `on_work_after` runs on `nessa-io-reactor` after `on_work_exec` completed.
    unsafe {
        let wctx_ptr =
            libuv_sys2::uv_req_get_data(req as *mut libuv_sys2::uv_req_t) as *mut WorkCtx;
        if wctx_ptr.is_null() {
            free_uv_aligned(req);
            return;
        }
        let mut wctx = Box::from_raw(wctx_ptr);
        let state = &mut *wctx.loop_state;
        state.works.remove(&wctx.op_id);
        if status != libuv_sys2::uv_errno_t_UV_ECANCELED
            && let Some(output) = wctx.output.take()
        {
            complete_entry(&wctx.entry, TaskOpResult::Work(output), &state.sched);
        }
        free_uv_aligned(wctx.req);
    }
}

// -- Reactor command handlers ------------------------------------------------

impl ReactorLoopState {
    fn handle_start_timer(&mut self, op_id: u64, duration_ms: u64, entry: Arc<TaskOpEntry>) {
        if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
            return;
        }
        // SAFETY: Reuse a pooled `uv_timer_t` or allocate exact `uv_handle_size(UV_TIMER)` bytes
        // and initialize on `self.loop_ptr`.
        unsafe {
            let ctx_ptr = if let Some(reused) = self.free_timers.pop() {
                (*reused).op_id = op_id;
                (*reused).entry = Some(Arc::clone(&entry));
                reused
            } else {
                let timer_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_TIMER);
                let handle = alloc_uv_aligned::<libuv_sys2::uv_timer_t>(timer_size);
                let rc = libuv_sys2::uv_timer_init(self.loop_ptr, handle);
                if rc < 0 {
                    free_uv_aligned(handle);
                    complete_entry(&entry, TaskOpResult::Unit, &self.sched);
                    return;
                }
                let fresh = Box::into_raw(Box::new(TimerCtx {
                    op_id,
                    handle,
                    loop_state: self as *mut Self,
                    entry: Some(Arc::clone(&entry)),
                    closing: false,
                }));
                libuv_sys2::uv_handle_set_data(
                    handle as *mut libuv_sys2::uv_handle_t,
                    fresh as *mut c_void,
                );
                fresh
            };
            let start_rc =
                libuv_sys2::uv_timer_start((*ctx_ptr).handle, Some(on_timer_fire), duration_ms, 0);
            if start_rc < 0 {
                (*ctx_ptr).entry = None;
                complete_entry(&entry, TaskOpResult::Unit, &self.sched);
                close_timer_ctx(ctx_ptr);
            } else {
                self.timers.insert(op_id, ctx_ptr);
            }
        }
    }

    fn handle_spawn_process(&mut self, op_id: u64, spec: ProcessSpec, entry: Arc<TaskOpEntry>) {
        if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
            return;
        }

        let Ok(prog_c) = CString::new(spec.prog.as_bytes()) else {
            complete_entry(
                &entry,
                TaskOpResult::Process(ProcessOutcome {
                    code: IO_INVALID_INPUT,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: "program path contains interior NUL byte".to_string(),
                }),
                &self.sched,
            );
            return;
        };

        let mut arg_cstrings = Vec::with_capacity(spec.args.len() + 1);
        arg_cstrings.push(prog_c.clone());
        for arg in &spec.args {
            let Ok(c_arg) = CString::new(arg.as_bytes()) else {
                complete_entry(
                    &entry,
                    TaskOpResult::Process(ProcessOutcome {
                        code: IO_INVALID_INPUT,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: "command argument contains interior NUL byte".to_string(),
                    }),
                    &self.sched,
                );
                return;
            };
            arg_cstrings.push(c_arg);
        }

        let cwd_c = match spec.cwd.as_deref().filter(|s| !s.is_empty()) {
            Some(dir) => match CString::new(dir.as_bytes()) {
                Ok(c) => Some(c),
                Err(_) => {
                    complete_entry(
                        &entry,
                        TaskOpResult::Process(ProcessOutcome {
                            code: IO_INVALID_INPUT,
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: "working directory contains interior NUL byte".to_string(),
                        }),
                        &self.sched,
                    );
                    return;
                }
            },
            None => None,
        };

        let mut argv_ptrs: Vec<*mut c_char> = arg_cstrings
            .iter()
            .map(|cs| cs.as_ptr() as *mut c_char)
            .collect();
        argv_ptrs.push(ptr::null_mut());

        // SAFETY: Allocate `uv_process_t` + `uv_pipe_t` handles using exact runtime sizes from
        // `uv_handle_size`, initialize pipes, invoke `uv_spawn`, and clean up via `uv_close`.
        unsafe {
            let pipe_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_NAMED_PIPE);
            let proc_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_PROCESS);

            let proc_handle = alloc_uv_aligned::<libuv_sys2::uv_process_t>(proc_size);
            let stdout_handle = alloc_uv_aligned::<libuv_sys2::uv_pipe_t>(pipe_size);
            let stderr_handle = alloc_uv_aligned::<libuv_sys2::uv_pipe_t>(pipe_size);
            let _ = libuv_sys2::uv_pipe_init(self.loop_ptr, stdout_handle, 0);
            let _ = libuv_sys2::uv_pipe_init(self.loop_ptr, stderr_handle, 0);

            let has_stdin = spec.stdin_data.is_some();
            let stdin_handle = if has_stdin {
                let h = alloc_uv_aligned::<libuv_sys2::uv_pipe_t>(pipe_size);
                let _ = libuv_sys2::uv_pipe_init(self.loop_ptr, h, 0);
                h
            } else {
                ptr::null_mut()
            };

            let open_handles = if has_stdin { 4 } else { 3 };
            let pctx_ptr = Box::into_raw(Box::new(ProcessCtx {
                op_id,
                proc_handle,
                proc_closing: false,
                stdout_pipe: ptr::null_mut(),
                stderr_pipe: ptr::null_mut(),
                stdin_pipe: ptr::null_mut(),
                stdout_done: false,
                stderr_done: false,
                exited: false,
                exit_status: -1,
                term_signal: 0,
                stdout_buf: Vec::new(),
                stderr_buf: Vec::new(),
                open_handles,
                completed: false,
                loop_state: self as *mut Self,
                entry: Arc::clone(&entry),
            }));

            let stdout_slot = Box::into_raw(Box::new(PipeSlot {
                role: PipeRole::Stdout,
                handle: stdout_handle,
                proc_ctx: pctx_ptr,
                closing: false,
            }));
            let stderr_slot = Box::into_raw(Box::new(PipeSlot {
                role: PipeRole::Stderr,
                handle: stderr_handle,
                proc_ctx: pctx_ptr,
                closing: false,
            }));
            let stdin_slot = if has_stdin {
                Box::into_raw(Box::new(PipeSlot {
                    role: PipeRole::Stdin,
                    handle: stdin_handle,
                    proc_ctx: pctx_ptr,
                    closing: false,
                }))
            } else {
                ptr::null_mut()
            };

            (*pctx_ptr).stdout_pipe = stdout_slot;
            (*pctx_ptr).stderr_pipe = stderr_slot;
            (*pctx_ptr).stdin_pipe = stdin_slot;

            libuv_sys2::uv_handle_set_data(
                proc_handle as *mut libuv_sys2::uv_handle_t,
                pctx_ptr as *mut c_void,
            );
            libuv_sys2::uv_handle_set_data(
                stdout_handle as *mut libuv_sys2::uv_handle_t,
                stdout_slot as *mut c_void,
            );
            libuv_sys2::uv_handle_set_data(
                stderr_handle as *mut libuv_sys2::uv_handle_t,
                stderr_slot as *mut c_void,
            );
            if has_stdin {
                libuv_sys2::uv_handle_set_data(
                    stdin_handle as *mut libuv_sys2::uv_handle_t,
                    stdin_slot as *mut c_void,
                );
            }

            let stdin_container = if has_stdin {
                libuv_sys2::uv_stdio_container_t {
                    flags: libuv_sys2::uv_stdio_flags_UV_CREATE_PIPE
                        | libuv_sys2::uv_stdio_flags_UV_READABLE_PIPE,
                    data: libuv_sys2::uv_stdio_container_s__bindgen_ty_1 {
                        stream: stdin_handle as *mut libuv_sys2::uv_stream_t,
                    },
                }
            } else {
                libuv_sys2::uv_stdio_container_t {
                    flags: libuv_sys2::uv_stdio_flags_UV_IGNORE,
                    data: libuv_sys2::uv_stdio_container_s__bindgen_ty_1 { fd: -1 },
                }
            };
            let stdout_container = libuv_sys2::uv_stdio_container_t {
                flags: libuv_sys2::uv_stdio_flags_UV_CREATE_PIPE
                    | libuv_sys2::uv_stdio_flags_UV_WRITABLE_PIPE,
                data: libuv_sys2::uv_stdio_container_s__bindgen_ty_1 {
                    stream: stdout_handle as *mut libuv_sys2::uv_stream_t,
                },
            };
            let stderr_container = libuv_sys2::uv_stdio_container_t {
                flags: libuv_sys2::uv_stdio_flags_UV_CREATE_PIPE
                    | libuv_sys2::uv_stdio_flags_UV_WRITABLE_PIPE,
                data: libuv_sys2::uv_stdio_container_s__bindgen_ty_1 {
                    stream: stderr_handle as *mut libuv_sys2::uv_stream_t,
                },
            };
            let mut stdio = [stdin_container, stdout_container, stderr_container];

            #[cfg(unix)]
            let flags: c_uint = libuv_sys2::uv_process_flags_UV_PROCESS_DETACHED;
            #[cfg(not(unix))]
            let flags: c_uint = 0;

            let options = libuv_sys2::uv_process_options_t {
                exit_cb: Some(on_proc_exit),
                file: prog_c.as_ptr(),
                args: argv_ptrs.as_mut_ptr(),
                env: ptr::null_mut(),
                cwd: cwd_c.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
                flags,
                stdio_count: 3,
                stdio: stdio.as_mut_ptr(),
                uid: 0,
                gid: 0,
            };

            let spawn_rc = libuv_sys2::uv_spawn(self.loop_ptr, proc_handle, &options);
            if spawn_rc < 0 {
                (*pctx_ptr).completed = true;
                complete_entry(
                    &entry,
                    TaskOpResult::Process(ProcessOutcome {
                        code: uv_error_to_io_code(spawn_rc),
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: uv_error_message(spawn_rc),
                    }),
                    &self.sched,
                );
                close_pipe_slot(stdin_slot);
                close_pipe_slot(stdout_slot);
                close_pipe_slot(stderr_slot);
                close_proc_handle(pctx_ptr);
                return;
            }

            let pid = libuv_sys2::uv_process_get_pid(proc_handle);
            entry.spawned_pid.store(pid, Ordering::Release);

            // Re-check if task was cancelled concurrently while `uv_spawn` was running.
            if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
                self.processes.insert(op_id, pctx_ptr);
                cancel_process_ctx(pctx_ptr);
                return;
            }

            self.processes.insert(op_id, pctx_ptr);

            let _ = libuv_sys2::uv_read_start(
                stdout_handle as *mut libuv_sys2::uv_stream_t,
                Some(on_pipe_alloc),
                Some(on_pipe_read),
            );
            let _ = libuv_sys2::uv_read_start(
                stderr_handle as *mut libuv_sys2::uv_stream_t,
                Some(on_pipe_alloc),
                Some(on_pipe_read),
            );

            if let Some(stdin_bytes) = spec.stdin_data {
                if stdin_bytes.is_empty() {
                    close_pipe_slot(stdin_slot);
                } else {
                    let write_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_WRITE);
                    let write_req = alloc_uv_aligned::<libuv_sys2::uv_write_t>(write_size);
                    let mut owned = stdin_bytes;
                    let uv_buf = libuv_sys2::uv_buf_t {
                        base: owned.as_mut_ptr() as *mut c_char,
                        len: owned.len(),
                    };
                    let wctx_ptr = Box::into_raw(Box::new(StdinWriteCtx {
                        req: write_req,
                        _data: owned,
                        proc_ctx: pctx_ptr,
                    }));
                    (*pctx_ptr).open_handles += 1;
                    libuv_sys2::uv_req_set_data(
                        write_req as *mut libuv_sys2::uv_req_t,
                        wctx_ptr as *mut c_void,
                    );
                    let wrc = libuv_sys2::uv_write(
                        write_req,
                        stdin_handle as *mut libuv_sys2::uv_stream_t,
                        &uv_buf,
                        1,
                        Some(on_stdin_write),
                    );
                    if wrc < 0 {
                        let wctx = Box::from_raw(wctx_ptr);
                        free_uv_aligned(wctx.req);
                        close_pipe_slot(stdin_slot);
                        decrement_process_open_handles(pctx_ptr);
                    }
                }
            }
        }
    }

    fn handle_queue_work(&mut self, op_id: u64, work: BoxedWorkFn, entry: Arc<TaskOpEntry>) {
        if matches!(*entry.state.lock(), TaskOpState::Cancelled) {
            return;
        }
        // SAFETY: Allocate `uv_work_t` of exact `uv_req_size(UV_WORK)` bytes and submit to loop.
        unsafe {
            let req_size = libuv_sys2::uv_req_size(libuv_sys2::uv_req_type_UV_WORK);
            let req = alloc_uv_aligned::<libuv_sys2::uv_work_t>(req_size);
            let wctx_ptr = Box::into_raw(Box::new(WorkCtx {
                op_id,
                req,
                loop_state: self as *mut Self,
                entry: Arc::clone(&entry),
                work: Some(work),
                output: None,
            }));
            libuv_sys2::uv_req_set_data(req as *mut libuv_sys2::uv_req_t, wctx_ptr as *mut c_void);
            let rc = libuv_sys2::uv_queue_work(
                self.loop_ptr,
                req,
                Some(on_work_exec),
                Some(on_work_after),
            );
            if rc < 0 {
                let mut wctx = Box::from_raw(wctx_ptr);
                free_uv_aligned(req);
                if let Some(fallback) = wctx.work.take() {
                    let out = fallback();
                    complete_entry(&entry, TaskOpResult::Work(out), &self.sched);
                }
            } else {
                self.works.insert(op_id, req);
            }
        }
    }

    fn handle_cancel_op(&mut self, op_id: u64) {
        if let Some(tctx_ptr) = self.timers.remove(&op_id) {
            recycle_or_close_timer_ctx(self, tctx_ptr);
        }
        if let Some(&pctx_ptr) = self.processes.get(&op_id) {
            // SAFETY: `pctx_ptr` is owned on the reactor thread and removed on handle drain.
            unsafe { cancel_process_ctx(pctx_ptr) };
        }
        if let Some(work_req) = self.works.get(&op_id).copied() {
            // SAFETY: `uv_cancel` cancels queued `uv_work_t` requests if they haven't started yet;
            // `on_work_after` still runs and frees `work_req`.
            unsafe {
                let _ = libuv_sys2::uv_cancel(work_req as *mut libuv_sys2::uv_req_t);
            }
        }
        let _ = self.net.cancel_op(op_id);
    }

    fn begin_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.net.begin_shutdown();
        let timer_ptrs: Vec<*mut TimerCtx> = self.timers.drain().map(|(_, p)| p).collect();
        for tptr in timer_ptrs {
            close_timer_ctx(tptr);
        }
        let pooled_timers = std::mem::take(&mut self.free_timers);
        for tptr in pooled_timers {
            close_timer_ctx(tptr);
        }
        let proc_ptrs: Vec<*mut ProcessCtx> = self.processes.drain().map(|(_, p)| p).collect();
        for pptr in proc_ptrs {
            // SAFETY: Reactor thread owns `pptr`.
            unsafe { cancel_process_ctx(pptr) };
        }
        for &work_req in self.works.values() {
            // SAFETY: Best-effort cancel of pending threadpool items during shutdown.
            unsafe {
                let _ = libuv_sys2::uv_cancel(work_req as *mut libuv_sys2::uv_req_t);
            }
        }
        // SAFETY: Close the doorbell `uv_async_t` so `uv_run(UV_RUN_DEFAULT)` exits once all
        // close and `after_work_cb` callbacks have drained.
        unsafe {
            libuv_sys2::uv_close(
                self.async_ptr as *mut libuv_sys2::uv_handle_t,
                Some(on_async_close),
            );
        }
    }
}

fn reactor_thread_main(
    doorbell: Arc<Mutex<Doorbell>>,
    sched: Weak<SchedulerInner>,
    net_registry: Arc<NetRegistry>,
    init_tx: std::sync::mpsc::SyncSender<usize>,
) {
    // SAFETY: Allocate and initialize an isolated `uv_loop_t` and `uv_async_t` exclusively
    // driven on this `nessa-io-reactor` thread until `Shutdown` drains all handles.
    unsafe {
        let loop_size = libuv_sys2::uv_loop_size();
        let loop_ptr = alloc_uv_aligned::<libuv_sys2::uv_loop_t>(loop_size);
        let rc = libuv_sys2::uv_loop_init(loop_ptr);
        assert!(rc == 0, "uv_loop_init failed in reactor: {rc}");

        let async_size = libuv_sys2::uv_handle_size(libuv_sys2::uv_handle_type_UV_ASYNC);
        let async_ptr = alloc_uv_aligned::<libuv_sys2::uv_async_t>(async_size);
        let arc = libuv_sys2::uv_async_init(loop_ptr, async_ptr, Some(on_async_doorbell));
        assert!(arc == 0, "uv_async_init failed in reactor: {arc}");

        let net_state = ReactorNetState::new(loop_ptr, Weak::clone(&sched), net_registry);
        let state_ptr = Box::into_raw(Box::new(ReactorLoopState {
            loop_ptr,
            async_ptr,
            doorbell,
            sched,
            timers: FastHashMap::default(),
            free_timers: Vec::new(),
            processes: FastHashMap::default(),
            works: FastHashMap::default(),
            net: net_state,
            shutting_down: false,
        }));

        libuv_sys2::uv_handle_set_data(
            async_ptr as *mut libuv_sys2::uv_handle_t,
            state_ptr as *mut c_void,
        );

        let _ = init_tx.send(async_ptr as usize);

        loop {
            if std::ptr::addr_of!((*state_ptr).shutting_down).read() {
                break;
            }
            let _ = libuv_sys2::uv_run(loop_ptr, libuv_sys2::uv_run_mode_UV_RUN_ONCE);
        }

        // Drain all remaining `uv_close` and `after_work_cb` callbacks cleanly.
        while libuv_sys2::uv_loop_alive(loop_ptr) != 0 {
            let _ = libuv_sys2::uv_run(loop_ptr, libuv_sys2::uv_run_mode_UV_RUN_DEFAULT);
        }
        let _ = libuv_sys2::uv_loop_close(loop_ptr);
        free_uv_aligned(loop_ptr);
        drop(Box::from_raw(state_ptr));
    }
}
