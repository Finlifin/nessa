use nsbc::FuncId;
use runtime::{TaskId, TaskState, TaskStatus};
use stack_pool::StackPool;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// EventLoop — safe wrapper around libuv's uv_loop_t
// ---------------------------------------------------------------------------

/// Run mode for the libuv event loop.
pub enum RunMode {
    /// Run until no active handles/requests.
    Default,
    /// Poll once, may block.
    Once,
    /// Poll once, never block.
    NoWait,
}

impl RunMode {
    fn to_uv(self) -> libuv_sys2::uv_run_mode {
        match self {
            RunMode::Default => libuv_sys2::uv_run_mode_UV_RUN_DEFAULT,
            RunMode::Once => libuv_sys2::uv_run_mode_UV_RUN_ONCE,
            RunMode::NoWait => libuv_sys2::uv_run_mode_UV_RUN_NOWAIT,
        }
    }
}

/// Thin safe wrapper around a libuv event loop (`uv_loop_t`).
pub struct EventLoop {
    handle: *mut libuv_sys2::uv_loop_t,
}

impl EventLoop {
    /// Create a new event loop.
    pub fn new() -> Self {
        unsafe {
            let handle = libuv_sys2::uv_loop_new();
            assert!(!handle.is_null(), "failed to allocate uv_loop_t");
            let rc = libuv_sys2::uv_loop_init(handle);
            assert!(rc == 0, "uv_loop_init failed: {rc}");
            Self { handle }
        }
    }

    /// Run the event loop with the given mode. Returns the uv_run status.
    pub fn run(&mut self, mode: RunMode) -> i32 {
        unsafe { libuv_sys2::uv_run(self.handle, mode.to_uv()) }
    }

    /// Check if the loop has active handles or requests.
    pub fn is_alive(&self) -> bool {
        unsafe { libuv_sys2::uv_loop_alive(self.handle) != 0 }
    }

    /// Stop the event loop.
    pub fn stop(&mut self) {
        unsafe { libuv_sys2::uv_stop(self.handle) }
    }

    /// Return the current timestamp in milliseconds.
    pub fn now(&self) -> u64 {
        unsafe { libuv_sys2::uv_now(self.handle) }
    }

    /// Get the raw loop pointer (for registering handles).
    pub fn raw(&mut self) -> *mut libuv_sys2::uv_loop_t {
        self.handle
    }
}

impl Drop for EventLoop {
    fn drop(&mut self) {
        unsafe {
            // Drain any pending callbacks.
            libuv_sys2::uv_run(self.handle, libuv_sys2::uv_run_mode_UV_RUN_NOWAIT);
            // uv_loop_delete closes + frees in one shot (asserts close == 0).
            libuv_sys2::uv_loop_delete(self.handle);
        }
    }
}

// SAFETY: The loop is only used from the scheduler's thread.
unsafe impl Send for EventLoop {}

// ---------------------------------------------------------------------------
// Scheduler — work-stealing task scheduler with libuv event loop
// ---------------------------------------------------------------------------

/// A cooperative work-stealing scheduler.
///
/// In the final implementation each worker thread has a local deque;
/// idle workers steal from others.  This simplified single-threaded
/// version captures the core API and integrates a libuv event loop
/// for asynchronous I/O.
pub struct Scheduler {
    /// Ready queue of tasks.
    ready_queue: VecDeque<TaskId>,
    /// All known tasks.
    tasks: Vec<TaskState>,
    /// Counter for generating unique TaskIds.
    next_id: AtomicU64,
    /// The libuv event loop for async I/O.
    event_loop: EventLoop,
    /// Task stack pool (mmap'd segmented stacks).
    stack_pool: Arc<StackPool>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::with_stack_pool(Arc::new(StackPool::new()))
    }

    /// Create a scheduler with a shared stack pool.
    pub fn with_stack_pool(stack_pool: Arc<StackPool>) -> Self {
        Self {
            ready_queue: VecDeque::new(),
            tasks: Vec::new(),
            next_id: AtomicU64::new(0),
            event_loop: EventLoop::new(),
            stack_pool,
        }
    }

    /// Get a reference to the stack pool.
    pub fn stack_pool(&self) -> &Arc<StackPool> {
        &self.stack_pool
    }

    /// Access the event loop.
    pub fn event_loop(&self) -> &EventLoop {
        &self.event_loop
    }

    /// Access the event loop mutably.
    pub fn event_loop_mut(&mut self) -> &mut EventLoop {
        &mut self.event_loop
    }

    /// Poll for I/O events without blocking.
    /// Returns `true` if the event loop has active handles/requests.
    pub fn poll_io(&mut self) -> bool {
        self.event_loop.run(RunMode::NoWait) != 0
    }

    /// Spawn a new task that will execute the given function.
    /// Returns the new task's id.
    pub fn spawn(&mut self, func_id: FuncId, parent: Option<TaskId>) -> TaskId {
        let id = TaskId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let mut task = TaskState::new(id, func_id);
        task.parent = parent;
        task.status = TaskStatus::Ready;
        task.stack = Some(self.stack_pool.alloc());

        // Register as child of parent.
        if let Some(pid) = parent {
            if let Some(ptask) = self.find_task_mut(pid) {
                ptask.children.push(id);
            }
        }

        self.tasks.push(task);
        self.ready_queue.push_back(id);
        id
    }

    /// Pick the next ready task to run.
    pub fn next_ready(&mut self) -> Option<TaskId> {
        while let Some(id) = self.ready_queue.pop_front() {
            if let Some(task) = self.find_task(id) {
                if task.status == TaskStatus::Ready {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Mark a task as running.
    pub fn set_running(&mut self, id: TaskId) {
        if let Some(task) = self.find_task_mut(id) {
            task.status = TaskStatus::Running;
        }
    }

    /// Suspend a task (e.g. waiting on I/O or effect).
    pub fn suspend(&mut self, id: TaskId) {
        if let Some(task) = self.find_task_mut(id) {
            task.status = TaskStatus::Suspended;
        }
    }

    /// Resume a suspended/waiting task.
    pub fn resume(&mut self, id: TaskId) {
        if let Some(task) = self.find_task_mut(id) {
            if task.status == TaskStatus::Suspended || task.status == TaskStatus::Waiting {
                task.status = TaskStatus::Ready;
                self.ready_queue.push_back(id);
            }
        }
    }

    /// Mark a task as finished and cancel its children.
    pub fn finish(&mut self, id: TaskId) {
        // Collect children first.
        let children = if let Some(task) = self.find_task(id) {
            task.children.clone()
        } else {
            return;
        };

        // Cancel all children (structured concurrency).
        for child_id in children {
            self.cancel(child_id);
        }

        if let Some(task) = self.find_task_mut(id) {
            task.status = TaskStatus::Finished;
            // Release the stack slot back to the pool.
            if let Some(handle) = task.stack.take() {
                self.stack_pool.dealloc(&handle);
            }
        }
    }

    /// Cancel a task and all its children.
    pub fn cancel(&mut self, id: TaskId) {
        let children = if let Some(task) = self.find_task(id) {
            task.children.clone()
        } else {
            return;
        };
        for child_id in children {
            self.cancel(child_id);
        }
        if let Some(task) = self.find_task_mut(id) {
            task.status = TaskStatus::Finished;
            if let Some(handle) = task.stack.take() {
                self.stack_pool.dealloc(&handle);
            }
        }
    }

    // -- Queries -------------------------------------------------------------

    pub fn get_task(&self, id: TaskId) -> Option<&TaskState> {
        self.find_task(id)
    }

    pub fn get_task_mut(&mut self, id: TaskId) -> Option<&mut TaskState> {
        self.find_task_mut(id)
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn ready_count(&self) -> usize {
        self.ready_queue.len()
    }

    /// Iterate over all tasks (for GC root scanning).
    pub fn all_tasks(&self) -> &[TaskState] {
        &self.tasks
    }

    // -- Internal helpers ----------------------------------------------------

    fn find_task(&self, id: TaskId) -> Option<&TaskState> {
        self.tasks.iter().find(|t| t.id == id)
    }

    fn find_task_mut(&mut self, id: TaskId) -> Option<&mut TaskState> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_and_schedule() {
        let mut sched = Scheduler::new();
        let t1 = sched.spawn(FuncId(0), None);
        let t2 = sched.spawn(FuncId(1), None);
        assert_eq!(sched.task_count(), 2);

        let next = sched.next_ready().unwrap();
        assert_eq!(next, t1);
        sched.set_running(next);

        let next2 = sched.next_ready().unwrap();
        assert_eq!(next2, t2);
    }

    #[test]
    fn structured_concurrency() {
        let mut sched = Scheduler::new();
        let parent = sched.spawn(FuncId(0), None);
        let child1 = sched.spawn(FuncId(1), Some(parent));
        let child2 = sched.spawn(FuncId(2), Some(parent));

        sched.finish(parent);
        assert_eq!(sched.get_task(child1).unwrap().status, TaskStatus::Finished);
        assert_eq!(sched.get_task(child2).unwrap().status, TaskStatus::Finished);
    }

    #[test]
    fn suspend_resume() {
        let mut sched = Scheduler::new();
        let t = sched.spawn(FuncId(0), None);
        sched.next_ready();
        sched.set_running(t);
        sched.suspend(t);
        assert_eq!(sched.get_task(t).unwrap().status, TaskStatus::Suspended);
        sched.resume(t);
        assert_eq!(sched.get_task(t).unwrap().status, TaskStatus::Ready);
        assert!(sched.next_ready().is_some());
    }
}
