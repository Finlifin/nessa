use std::cell::UnsafeCell;

use gc::{GcConfig, NessaSlot, RootRegistration, gc_init};
use type_pool::Intrinsic;

#[test]
fn parallel_mutators_allocate_and_survive_concurrent_collections() {
    struct WorkerRoot {
        slot: UnsafeCell<u64>,
    }
    // SAFETY: Each WorkerRoot is mutated exclusively by its owner thread while inside
    // a MutatorSession, and scanned by MMTk exclusively during stop-the-world epochs.
    unsafe impl Sync for WorkerRoot {}

    unsafe fn scan_worker_root(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) {
        // SAFETY: `context` points to a live boxed `WorkerRoot` for the registration's lifetime.
        let root = unsafe { &*context.cast::<WorkerRoot>() };
        // SAFETY: STW guarantees all mutator sessions are stopped and aligned u64 is valid.
        visitor(unsafe { NessaSlot::from_raw_ptr(root.slot.get().cast_const()) });
    }

    let mut workers = Vec::new();
    for _ in 0..4 {
        let root = Box::new(WorkerRoot {
            slot: UnsafeCell::new(1), // tagged immediate initially
        });
        // SAFETY: Box keeps `root` at a stable address and registration drops before `root`.
        let reg = unsafe {
            RootRegistration::new((&*root as *const WorkerRoot).cast(), scan_worker_root)
        };
        let heap = gc_init(&GcConfig::default());
        workers.push((reg, root, heap));
    }

    std::thread::scope(|s| {
        for (worker_idx, (_reg, root, heap)) in workers.iter_mut().enumerate() {
            let root_ref: &WorkerRoot = root;
            s.spawn(move || {
                for iter in 0..400u64 {
                    // SAFETY: `heap` is owned exclusively by this worker thread; all live
                    // references are published to `root_ref.slot` before polling/collecting.
                    let _session = unsafe { heap.begin_operation() };
                    let payload = unsafe {
                        heap.alloc_object(Intrinsic::Str.type_index(), 3)
                            .expect("parallel mutator allocation")
                    };
                    let expected_tag = ((worker_idx as u64) << 32) | iter;
                    // SAFETY: 3-word Str payload is freshly allocated and exclusively owned.
                    unsafe {
                        let words = payload.as_ptr().cast::<u64>();
                        words.write(8);
                        words.add(1).write(expected_tag);
                        words.add(2).write(!expected_tag);
                        *root_ref.slot.get() = payload.as_ptr() as u64;
                    }
                    if iter % 50 == 0 && worker_idx == 0 {
                        // SAFETY: all mutator roots are published in `root_ref.slot`.
                        unsafe {
                            heap.collect_garbage();
                        }
                    } else {
                        heap.safe_point();
                    }
                    // Re-read from registered root slot in case Immix relocated the object!
                    // SAFETY: `root_ref.slot` was updated by GC if moved and session is active.
                    unsafe {
                        let current_ptr = (*root_ref.slot.get()) as *const u64;
                        assert_ne!(current_ptr as usize, 0);
                        assert_eq!(current_ptr.read(), 8);
                        assert_eq!(current_ptr.add(1).read(), expected_tag);
                        assert_eq!(current_ptr.add(2).read(), !expected_tag);
                    }
                }
            });
        }
    });
}
