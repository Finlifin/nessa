//! Mutator lifecycle and stop-the-world epochs.
//!
//! An operation publishes all of its roots before becoming idle. Collection
//! waits for every running operation to stop, and freezes registration and
//! destruction until its snapshot has been released.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Condvar, Mutex};
use std::thread::ThreadId;

use mmtk::Mutator;
use mmtk::util::opaque_pointer::{VMMutatorThread, VMThread};

use crate::{GC_FLAGS, NessaVM};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Normal,
    Stopping,
    Collecting,
}

struct Entry {
    pointer: usize,
    depth: usize,
    stopped: bool,
    owner: Option<ThreadId>,
}

struct Registry {
    entries: BTreeMap<usize, Entry>,
    phase: Phase,
    epoch: u64,
    completed: u64,
    lifecycles: usize,
}

struct Coordinator {
    registry: Mutex<Registry>,
    changed: Condvar,
}

impl Coordinator {
    const fn new() -> Self {
        Self {
            registry: Mutex::new(Registry {
                entries: BTreeMap::new(),
                phase: Phase::Normal,
                epoch: 0,
                completed: 0,
                lifecycles: 0,
            }),
            changed: Condvar::new(),
        }
    }

    fn normal_registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        let mut registry = self.registry.lock().unwrap();
        while registry.phase != Phase::Normal && !Self::thread_is_running(&registry) {
            registry = self.changed.wait(registry).unwrap();
        }
        registry
    }

    fn thread_is_running(registry: &Registry) -> bool {
        let owner = std::thread::current().id();
        registry
            .entries
            .values()
            .any(|entry| entry.owner == Some(owner) && entry.depth != 0 && !entry.stopped)
    }

    fn lifecycle(&self) -> MutatorLifecycle<'_> {
        self.normal_registry().lifecycles += 1;
        MutatorLifecycle {
            coordinator: self,
            _thread_bound: PhantomData,
        }
    }

    #[cfg(test)]
    fn register(&self, identity: usize, pointer: usize) {
        let lifecycle = self.lifecycle();
        lifecycle.register_raw(identity, pointer);
    }

    #[cfg(test)]
    fn unregister(&self, identity: usize) {
        self.lifecycle().unregister_raw(identity);
    }

    fn register_reserved(&self, identity: usize, pointer: usize) {
        let previous = self.registry.lock().unwrap().entries.insert(
            identity,
            Entry {
                pointer,
                depth: 0,
                stopped: false,
                owner: None,
            },
        );
        assert!(previous.is_none(), "mutator identity registered twice");
    }

    fn unregister_reserved(&self, identity: usize) {
        let mut registry = self.registry.lock().unwrap();
        let entry = registry
            .entries
            .get(&identity)
            .expect("mutator is registered");
        assert_eq!(
            entry.depth, 0,
            "cannot destroy a mutator during an operation"
        );
        registry.entries.remove(&identity);
    }

    fn begin(&self, identity: usize) -> MutatorSession<'_> {
        let mut registry = self.registry.lock().unwrap();
        loop {
            let entry = registry
                .entries
                .get(&identity)
                .expect("mutator is registered");
            if entry.depth != 0
                || registry.phase == Phase::Normal
                || Self::thread_is_running(&registry)
            {
                break;
            }
            registry = self.changed.wait(registry).unwrap();
        }
        let entry = registry.entries.get_mut(&identity).unwrap();
        let owner = std::thread::current().id();
        if entry.depth != 0 {
            assert_eq!(
                entry.owner,
                Some(owner),
                "mutator operations must be exclusive"
            );
            assert!(!entry.stopped, "cannot reenter a stopped mutator");
        }
        entry.owner = Some(owner);
        entry.depth += 1;
        MutatorSession {
            coordinator: self,
            identity,
            _thread_bound: PhantomData,
        }
    }

    fn end(&self, identity: usize) {
        let mut registry = self.registry.lock().unwrap();
        let entry = registry
            .entries
            .get_mut(&identity)
            .expect("operation owns a live mutator");
        assert_eq!(entry.owner, Some(std::thread::current().id()));
        assert!(!entry.stopped);
        entry.depth -= 1;
        if entry.depth == 0 {
            entry.owner = None;
            self.changed.notify_all();
        }
    }

    fn stop_all(&self) -> Vec<usize> {
        let mut registry = self.registry.lock().unwrap();
        assert_eq!(registry.phase, Phase::Normal, "collections cannot overlap");
        registry.epoch += 1;
        registry.phase = Phase::Stopping;
        self.changed.notify_all();
        while registry.lifecycles != 0
            || registry
                .entries
                .values()
                .any(|entry| entry.depth != 0 && !entry.stopped)
        {
            registry = self.changed.wait(registry).unwrap();
        }
        registry.phase = Phase::Collecting;
        registry
            .entries
            .values()
            .map(|entry| entry.pointer)
            .collect()
    }

    fn resume(&self) {
        let mut registry = self.registry.lock().unwrap();
        assert_eq!(registry.phase, Phase::Collecting);
        registry.completed = registry.epoch;
        registry.phase = Phase::Normal;
        self.changed.notify_all();
    }

    fn park(&self, identity: usize, allocation_request: bool) {
        let mut registry = self.registry.lock().unwrap();
        // A poll can race resume. It must not accidentally wait for the next GC.
        if !allocation_request && registry.phase == Phase::Normal {
            return;
        }
        // Allocation can block before the collector enters Stopping.
        let target = registry.epoch + u64::from(registry.phase == Phase::Normal);
        let entry = registry
            .entries
            .get_mut(&identity)
            .expect("mutator is registered");
        assert_ne!(entry.depth, 0, "GC blocking requires an active operation");
        assert_eq!(entry.owner, Some(std::thread::current().id()));
        entry.stopped = true;
        self.changed.notify_all();
        while registry.completed < target || registry.phase != Phase::Normal {
            registry = self.changed.wait(registry).unwrap();
        }
        registry.entries.get_mut(&identity).unwrap().stopped = false;
    }
}

static COORDINATOR: Coordinator = Coordinator::new();

/// A running mutator operation. Its roots must be published before it ends.
/// This guard is bound to its entering thread, including for nested operations.
pub struct MutatorSession<'a> {
    coordinator: &'a Coordinator,
    identity: usize,
    _thread_bound: PhantomData<Rc<()>>,
}

/// Reserves mutator construction/destruction while a collector starts waiting.
pub(crate) struct MutatorLifecycle<'a> {
    coordinator: &'a Coordinator,
    _thread_bound: PhantomData<Rc<()>>,
}

impl MutatorLifecycle<'_> {
    fn register_raw(&self, identity: usize, pointer: usize) {
        self.coordinator.register_reserved(identity, pointer);
    }
    fn unregister_raw(&self, identity: usize) {
        self.coordinator.unregister_reserved(identity);
    }
    pub(crate) fn register(&self, tls: VMMutatorThread, pointer: *mut Mutator<NessaVM>) {
        self.register_raw(identity(tls.0), pointer as usize);
    }
    pub(crate) fn unregister(&self, tls: VMMutatorThread) {
        self.unregister_raw(identity(tls.0));
    }
}

impl Drop for MutatorLifecycle<'_> {
    fn drop(&mut self) {
        let mut registry = self.coordinator.registry.lock().unwrap();
        registry.lifecycles -= 1;
        self.coordinator.changed.notify_all();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MutatorAccessError {
    CrossMutatorReentry,
}

impl Drop for MutatorSession<'_> {
    fn drop(&mut self) {
        self.coordinator.end(self.identity);
    }
}

fn identity(tls: VMThread) -> usize {
    tls.0.to_address().as_usize()
}

pub(crate) fn lifecycle() -> MutatorLifecycle<'static> {
    COORDINATOR.lifecycle()
}

pub(crate) fn begin(tls: VMMutatorThread) -> MutatorSession<'static> {
    COORDINATOR.begin(identity(tls.0))
}

pub(crate) fn begin_execution(
    tls: VMMutatorThread,
) -> Result<MutatorSession<'static>, MutatorAccessError> {
    let owner = std::thread::current().id();
    let nested = COORDINATOR
        .registry
        .lock()
        .unwrap()
        .entries
        .iter()
        .any(|(&id, entry)| {
            id != identity(tls.0) && entry.owner == Some(owner) && entry.depth != 0
        });
    if nested {
        return Err(MutatorAccessError::CrossMutatorReentry);
    }
    Ok(begin(tls))
}

pub(crate) fn poll(tls: VMMutatorThread) {
    COORDINATOR.park(identity(tls.0), false);
}

pub(crate) fn block(tls: VMMutatorThread) {
    COORDINATOR.park(identity(tls.0), true);
}

pub(crate) fn stop_all() -> Vec<*mut Mutator<NessaVM>> {
    // Publish the request before waiting, so running VM loops enter poll.
    GC_FLAGS.gc_requested.store(true, Ordering::Release);
    COORDINATOR
        .stop_all()
        .into_iter()
        .map(|pointer| pointer as *mut Mutator<NessaVM>)
        .collect()
}

pub(crate) fn resume() {
    // Clear before reopening the epoch. A late poll consults the phase as well.
    GC_FLAGS.gc_requested.store(false, Ordering::Release);
    COORDINATOR.resume();
}

pub(crate) fn contains(tls: VMThread) -> bool {
    COORDINATOR
        .registry
        .lock()
        .unwrap()
        .entries
        .contains_key(&identity(tls))
}

pub(crate) fn lookup(tls: VMMutatorThread) -> *mut Mutator<NessaVM> {
    COORDINATOR
        .registry
        .lock()
        .unwrap()
        .entries
        .get(&identity(tls.0))
        .expect("MMTk requested an unregistered mutator")
        .pointer as *mut Mutator<NessaVM>
}

pub(crate) fn pointers() -> Vec<*mut Mutator<NessaVM>> {
    COORDINATOR
        .registry
        .lock()
        .unwrap()
        .entries
        .values()
        .map(|entry| entry.pointer as *mut Mutator<NessaVM>)
        .collect()
}

pub(crate) fn count() -> usize {
    COORDINATOR.registry.lock().unwrap().entries.len()
}

pub(crate) fn completed_collections() -> u64 {
    COORDINATOR.registry.lock().unwrap().completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    fn wait_for(coordinator: &Coordinator, condition: impl Fn(&Registry) -> bool) {
        let mut registry = coordinator.registry.lock().unwrap();
        while !condition(&registry) {
            let (next, timeout) = coordinator
                .changed
                .wait_timeout(registry, Duration::from_secs(5))
                .unwrap();
            registry = next;
            assert!(
                !timeout.timed_out(),
                "coordinator did not reach expected state"
            );
        }
    }

    #[test]
    fn collection_waits_for_every_running_mutator() {
        let coordinator = Arc::new(Coordinator::new());
        coordinator.register(1, 101);
        coordinator.register(2, 202);
        coordinator.register(3, 303); // idle engines also belong to the snapshot
        let mut workers = Vec::new();
        let mut permits = Vec::new();
        for identity in [1, 2] {
            let coordinator = Arc::clone(&coordinator);
            let (started_tx, started_rx) = mpsc::channel();
            let (permit_tx, permit_rx) = mpsc::channel();
            workers.push(std::thread::spawn(move || {
                let _session = coordinator.begin(identity);
                started_tx.send(()).unwrap();
                permit_rx.recv().unwrap();
                coordinator.park(identity, false);
            }));
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            permits.push(permit_tx);
        }
        let (collected_tx, collected_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let collector = {
            let coordinator = Arc::clone(&coordinator);
            std::thread::spawn(move || {
                let snapshot = coordinator.stop_all();
                collected_tx.send(snapshot).unwrap();
                resume_rx.recv().unwrap();
                coordinator.resume();
            })
        };
        wait_for(&coordinator, |state| state.phase == Phase::Stopping);
        permits[0].send(()).unwrap();
        wait_for(&coordinator, |state| state.entries[&1].stopped);
        assert_eq!(coordinator.registry.lock().unwrap().phase, Phase::Stopping);
        assert!(collected_rx.try_recv().is_err());
        permits[1].send(()).unwrap();
        assert_eq!(
            collected_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            [101, 202, 303]
        );
        resume_tx.send(()).unwrap();
        collector.join().unwrap();
        for worker in workers {
            worker.join().unwrap();
        }
        let registry = coordinator.registry.lock().unwrap();
        assert_eq!(registry.completed, 1);
        assert!(
            registry
                .entries
                .values()
                .all(|entry| entry.depth == 0 && !entry.stopped)
        );
    }

    #[test]
    fn allocation_block_can_precede_collection_and_late_poll_returns() {
        let coordinator = Arc::new(Coordinator::new());
        coordinator.register(1, 101);
        let worker = {
            let coordinator = Arc::clone(&coordinator);
            std::thread::spawn(move || {
                let _session = coordinator.begin(1);
                coordinator.park(1, true);
                // GC has completed: an ordinary poll must not wait for epoch 2.
                coordinator.park(1, false);
            })
        };
        wait_for(&coordinator, |state| state.entries[&1].stopped);
        assert_eq!(coordinator.stop_all(), [101]);
        coordinator.resume();
        worker.join().unwrap();
        assert_eq!(coordinator.registry.lock().unwrap().completed, 1);
    }

    #[test]
    fn nested_operations_publish_idle_only_after_outer_drop() {
        let coordinator = Coordinator::new();
        coordinator.register(1, 101);
        let outer = coordinator.begin(1);
        let inner = coordinator.begin(1);
        assert_eq!(coordinator.registry.lock().unwrap().entries[&1].depth, 2);
        drop(inner);
        assert_eq!(coordinator.registry.lock().unwrap().entries[&1].depth, 1);
        drop(outer);
        assert_eq!(coordinator.stop_all(), [101]);
        coordinator.resume();
        coordinator.unregister(1);
        assert!(coordinator.registry.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn collection_waits_until_mutator_destruction_finishes() {
        let coordinator = Arc::new(Coordinator::new());
        coordinator.register(1, 101);
        let retirement = coordinator.lifecycle();
        retirement.unregister_raw(1);
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let collector = {
            let coordinator = Arc::clone(&coordinator);
            std::thread::spawn(move || {
                snapshot_tx.send(coordinator.stop_all()).unwrap();
                coordinator.resume();
            })
        };
        wait_for(&coordinator, |registry| registry.phase == Phase::Stopping);
        assert!(snapshot_rx.try_recv().is_err());
        drop(retirement); // allocator flush/on_destroy has now completed
        assert!(
            snapshot_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_empty()
        );
        collector.join().unwrap();
    }
}
