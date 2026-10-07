//! Completed collections distinguish raw stack roots from weak language owners.

use std::sync::Arc;

use gc::GcConfig;
use nsbc::{FuncId, Instruction, Reg};
use runtime::{CallFrame, ContinuationId, PromptId, StackContext, TaggedValue, TaskId, TaskStatus};
use stack_pool::StackPool;
use type_pool::TypePool;

use crate::{DispatchResult, Vm, builtin_ctx::heap_string_to_owned};

struct Fixture {
    // Release the operation before unregistering/dropping the VM's roots.
    _operation: gc::MutatorSession<'static>,
    vm: Vm,
    task: TaskId,
}

impl Fixture {
    fn new() -> Self {
        let mut vm = Vm::new(
            TypePool::with_intrinsics(),
            &GcConfig {
                heap_size: 8 * 1024 * 1024,
                plan: "Immix".into(),
                gc_threads: 2,
            },
            Arc::new(StackPool::new()),
        );
        let operation = vm.operation();
        let task = vm.spawn_root(FuncId(0));
        Self {
            _operation: operation,
            vm,
            task,
        }
    }

    fn task_mut(&mut self) -> &mut runtime::TaskState {
        self.vm.roots.scheduler.get_task_mut(self.task).unwrap()
    }

    fn raw_capture(&mut self, text: &str, depth: usize) -> ContinuationId {
        assert!(depth > 0);
        for index in 0..depth {
            self.task_mut()
                .stacks
                .enter_delimiter(PromptId(7 + index as u32), FuncId(0), &[])
                .unwrap();
        }
        let value = self.vm.alloc_string(text).unwrap();
        // Publish immediately: the Rust local is not an ordinary GC root.
        self.task_mut().registers.set(Reg(10), value);
        let capture = self.task_mut().stacks.capture(PromptId(7), Reg(3)).unwrap();
        // Exercise heap slots in every segment of nested delimiter chains.
        self.task_mut()
            .stacks
            .visit_captured_contexts_mut(capture, |context| {
                context.registers.set(Reg(10), value);
                context.local_slots.push(value);
            })
            .unwrap();
        capture
    }

    fn publish(&mut self, capture: ContinuationId, register: Reg) {
        let owner = self.vm.alloc_continuation(self.task, capture).unwrap();
        self.task_mut().registers.set(register, owner);
    }

    fn managed_capture(&mut self, text: &str, register: Reg) -> ContinuationId {
        let capture = self.raw_capture(text, 1);
        self.publish(capture, register);
        capture
    }

    fn register(&self, register: Reg) -> TaggedValue {
        self.vm
            .roots
            .scheduler
            .get_task(self.task)
            .unwrap()
            .registers
            .get(register)
    }

    fn edge(&mut self, capture: ContinuationId, owner_register: Reg) {
        let owner = self.register(owner_register);
        self.task_mut()
            .stacks
            .visit_captured_contexts_mut(capture, |context| {
                context.registers.set(Reg(5), owner);
            })
            .unwrap();
    }

    fn clear_ordinary_roots(&mut self) {
        let context = self.task_mut().stacks.active_mut();
        context.registers.regs.fill(TaggedValue::UNIT);
        context.local_slots.clear();
        context.call_stack.clear();
        context.handler_stack.clear();
        context.entry_closure_env = None;
        context.display_state = None;
        self.vm.roots.temporary_values.borrow_mut().clear();
        assert!(self.vm.roots.constants.values().is_empty());
        assert!(self.vm.roots.globals.values().is_empty());
    }

    fn collect(&mut self) {
        let before = self.vm.completed_collections();
        assert!(self.vm.collect_garbage().unwrap());
        assert!(self.vm.completed_collections() > before);
    }

    fn assert_counts(&self, captured: usize, allocated: usize) {
        let task = self.vm.roots.scheduler.get_task(self.task).unwrap();
        assert!(matches!(task.status, TaskStatus::Ready));
        assert_eq!(task.stacks.captured_count(), captured);
        assert_eq!(
            self.vm.roots.scheduler.stack_pool().active_count(),
            allocated
        );
    }

    fn assert_payload(&mut self, capture: ContinuationId, text: &str, depth: usize) {
        let mut visited = 0;
        self.task_mut()
            .stacks
            .visit_captured_contexts_mut(capture, |context| {
                assert_eq!(
                    heap_string_to_owned(context.registers.get(Reg(10))).as_deref(),
                    Some(text)
                );
                assert_eq!(
                    heap_string_to_owned(context.local_slots[0]).as_deref(),
                    Some(text)
                );
                visited += 1;
            })
            .unwrap();
        assert_eq!(visited, depth);
    }

    fn has_capture(&self, capture: ContinuationId) -> bool {
        self.vm
            .roots
            .scheduler
            .get_task(self.task)
            .unwrap()
            .stacks
            .has_capture(capture)
    }

    fn root_context_count(&self) -> usize {
        self.vm
            .roots
            .scheduler
            .get_task(self.task)
            .unwrap()
            .stacks
            .root_contexts()
            .count()
    }

    fn captured_head_address(&mut self, capture: ContinuationId) -> usize {
        let mut address = None;
        self.task_mut()
            .stacks
            .visit_captured_contexts_mut(capture, |context| {
                address.get_or_insert(context as *const StackContext as usize);
            })
            .unwrap();
        address.unwrap()
    }

    fn owner_handle(&self, register: Reg) -> ContinuationId {
        let pointer = self.register(register).as_heap_ptr().unwrap();
        // SAFETY: this value was allocated as a one-word Continuation and is
        // freshly read from an ordinary VM root under the fixture operation.
        let payload = TaggedValue::from_raw(unsafe { pointer.cast::<u64>().read() });
        ContinuationId::from_u64(payload.as_u64().unwrap())
    }
}

#[test]
fn raw_captures_retain_heap_values_across_completed_collections() {
    let mut fixture = Fixture::new();
    let capture = fixture.raw_capture("raw capture keeps this heap string", 1);
    fixture.clear_ordinary_roots();
    assert_eq!(fixture.root_context_count(), 2);
    for _ in 0..3 {
        fixture.collect();
        fixture.assert_counts(1, 2);
        fixture.assert_payload(capture, "raw capture keeps this heap string", 1);
    }
    let address = fixture.captured_head_address(capture);
    fixture
        .task_mut()
        .stacks
        .resume(capture, TaggedValue::from_i64(42))
        .unwrap();
    assert_eq!(
        fixture.task_mut().stacks.active() as *const StackContext as usize,
        address
    );
    fixture.collect();
    fixture.assert_counts(0, 2);
    assert_eq!(fixture.register(Reg(3)).as_i64(), Some(42));
    assert_eq!(
        heap_string_to_owned(fixture.register(Reg(10))).as_deref(),
        Some("raw capture keeps this heap string")
    );
    assert!(
        fixture
            .task_mut()
            .stacks
            .return_from_delimiter(TaggedValue::from_i64(42))
    );
    fixture.assert_counts(0, 1);
}

#[test]
fn pending_raw_fork_survives_collection_of_its_managed_source_owner() {
    let mut fixture = Fixture::new();
    let source = fixture.managed_capture("pending fork payload", Reg(4));
    let pending = fixture.task_mut().stacks.fork(source).unwrap();
    fixture.clear_ordinary_roots();
    assert_eq!(fixture.root_context_count(), 2);
    fixture.collect();
    assert!(!fixture.has_capture(source));
    assert!(fixture.has_capture(pending));
    fixture.assert_counts(1, 2);
    fixture.assert_payload(pending, "pending fork payload", 1);
    fixture.publish(pending, Reg(5));
    assert_eq!(fixture.root_context_count(), 1);
    fixture.collect();
    fixture.assert_counts(1, 2);
    fixture.assert_payload(pending, "pending fork payload", 1);
    fixture.clear_ordinary_roots();
    fixture.collect();
    fixture.assert_counts(0, 1);
}

#[test]
fn managed_capture_is_reclaimed_only_after_all_ordinary_register_and_frame_roots_clear() {
    let mut fixture = Fixture::new();
    let capture = fixture.managed_capture("conservative ordinary roots", Reg(4));
    let owner = fixture.register(Reg(4));
    let context = fixture.task_mut().stacks.active_mut();
    context.local_slots.push(owner);
    context.call_stack.push(CallFrame {
        display_state: None,
        return_pc: 0,
        func_id: FuncId(0),
        saved_regs: vec![(Reg(9), owner)],
        local_slots: vec![owner],
        evidence: vec![],
        closure_env: Some(owner),
    });
    context.entry_closure_env = Some(owner);
    context.registers.regs.fill(TaggedValue::UNIT);
    fixture.collect();
    fixture.assert_counts(1, 2);
    fixture.assert_payload(capture, "conservative ordinary roots", 1);
    fixture.clear_ordinary_roots();
    fixture.collect();
    assert!(!fixture.has_capture(capture));
    fixture.assert_counts(0, 1);
    fixture.collect();
    fixture.assert_counts(0, 1);
}

#[test]
fn an_unreachable_template_self_cycle_releases_its_stack_before_task_finish() {
    let mut fixture = Fixture::new();
    let capture = fixture.managed_capture("self cycle payload", Reg(4));
    fixture.edge(capture, Reg(4));
    fixture.collect();
    fixture.assert_counts(1, 2);
    fixture.assert_payload(capture, "self cycle payload", 1);
    fixture.clear_ordinary_roots();
    fixture.collect();
    assert!(!fixture.has_capture(capture));
    fixture.assert_counts(0, 1);
}

#[test]
fn an_unreachable_cross_cycle_releases_both_templates_before_task_finish() {
    let mut fixture = Fixture::new();
    let first = fixture.managed_capture("cross cycle first", Reg(4));
    let second = fixture.managed_capture("cross cycle second", Reg(5));
    fixture.edge(first, Reg(5));
    fixture.edge(second, Reg(4));
    fixture.clear_ordinary_roots();
    fixture.collect();
    assert!(!fixture.has_capture(first));
    assert!(!fixture.has_capture(second));
    fixture.assert_counts(0, 1);
}

#[test]
fn an_externally_rooted_cross_cycle_survives_then_releases_after_its_last_root_clears() {
    let mut fixture = Fixture::new();
    let first = fixture.managed_capture("reachable cycle first", Reg(4));
    let second = fixture.managed_capture("reachable cycle second", Reg(5));
    fixture.edge(first, Reg(5));
    fixture.edge(second, Reg(4));
    fixture.task_mut().registers.set(Reg(5), TaggedValue::UNIT);
    assert_eq!(fixture.root_context_count(), 1);
    for _ in 0..3 {
        fixture.collect();
        fixture.assert_counts(2, 3);
        fixture.assert_payload(first, "reachable cycle first", 1);
        fixture.assert_payload(second, "reachable cycle second", 1);
        assert_eq!(fixture.owner_handle(Reg(4)), first);
    }
    fixture.clear_ordinary_roots();
    fixture.collect();
    fixture.assert_counts(0, 1);
}

#[test]
fn reachable_template_chain_traces_to_a_fixed_point_and_updates_heap_slots() {
    let mut fixture = Fixture::new();
    let first = fixture.managed_capture("chain first", Reg(4));
    let second = fixture.managed_capture("chain second", Reg(5));
    let third = fixture.managed_capture(
        "chain third has forty-two bytes of meaningful state",
        Reg(6),
    );
    fixture.edge(first, Reg(5));
    fixture.edge(second, Reg(6));
    fixture.task_mut().registers.set(Reg(5), TaggedValue::UNIT);
    fixture.task_mut().registers.set(Reg(6), TaggedValue::UNIT);
    for _ in 0..3 {
        fixture.collect();
        fixture.assert_counts(3, 4);
        fixture.assert_payload(first, "chain first", 1);
        fixture.assert_payload(second, "chain second", 1);
        fixture.assert_payload(
            third,
            "chain third has forty-two bytes of meaningful state",
            1,
        );
    }
    fixture.clear_ordinary_roots();
    fixture.collect();
    fixture.assert_counts(0, 1);
}

#[test]
fn cloned_owner_has_its_own_lifetime_and_once_resume_preserves_the_active_chain() {
    let mut fixture = Fixture::new();
    let source = fixture.raw_capture("nested cloned heap payload", 2);
    fixture.publish(source, Reg(4));
    let source_address = fixture.captured_head_address(source);
    let owner = fixture.register(Reg(4));
    assert!(matches!(
        fixture.vm.clone_language_continuation(fixture.task, owner),
        DispatchResult::Continue
    ));
    let clone = fixture.owner_handle(Reg(0));
    let clone_address = fixture.captured_head_address(clone);
    assert_ne!(source, clone);
    assert_ne!(source_address, clone_address);
    fixture.assert_counts(2, 5);
    fixture.task_mut().registers.set(Reg(4), TaggedValue::UNIT);
    fixture.collect();
    assert!(!fixture.has_capture(source));
    fixture.assert_counts(1, 3);
    fixture.assert_payload(clone, "nested cloned heap payload", 2);
    fixture
        .task_mut()
        .registers
        .set(Reg(1), TaggedValue::from_i64(42));
    assert!(matches!(
        fixture.vm.dispatch_language_resume(
            fixture.task,
            &Instruction::resume_continuation_once(Reg(0), Reg(1)),
        ),
        DispatchResult::Continue
    ));
    assert!(!fixture.has_capture(clone));
    assert_eq!(
        fixture.task_mut().stacks.active() as *const StackContext as usize,
        clone_address
    );
    fixture.collect();
    fixture.assert_counts(0, 3);
    assert_eq!(fixture.register(Reg(3)).as_i64(), Some(42));
    assert_eq!(
        heap_string_to_owned(fixture.register(Reg(10))).as_deref(),
        Some("nested cloned heap payload")
    );
    assert!(
        fixture
            .task_mut()
            .stacks
            .return_from_delimiter(TaggedValue::from_i64(42))
    );
    fixture.collect();
    fixture.assert_counts(0, 2);
    assert_eq!(
        heap_string_to_owned(fixture.register(Reg(10))).as_deref(),
        Some("nested cloned heap payload")
    );
    assert!(
        fixture
            .task_mut()
            .stacks
            .return_from_delimiter(TaggedValue::from_i64(42))
    );
    fixture.collect();
    fixture.assert_counts(0, 1);
    assert_eq!(fixture.register(Reg(0)).as_i64(), Some(42));
}

#[test]
fn weak_only_downstream_owner_and_captured_heap_slots_follow_actual_object_moves() {
    // MMTk is process-global. Other tests can pin allocation lines or choose
    // the initial heap size, so actual evacuation must use an isolated heap.
    const CHILD: &str = "NESSA_CONTINUATION_MOVEMENT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "continuation_gc_tests::weak_only_downstream_owner_and_captured_heap_slots_follow_actual_object_moves",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "isolated movement test: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut fixture = Fixture::new();
    let first = fixture.managed_capture("moving chain first", Reg(4));
    // Populate allocation lines with objects that will become garbage, so the
    // retained downstream owner and string occupy a fragmented Immix block.
    let garbage = "g".repeat(512);
    let live = "l".repeat(64);
    let gaps = |fixture: &mut Fixture, count| {
        for _ in 0..count {
            let value = fixture.vm.alloc_string(&garbage).unwrap();
            fixture.task_mut().registers.set(Reg(20), value);
            let value = fixture.vm.alloc_string(&live).unwrap();
            fixture.task_mut().registers.set(Reg(20), value);
            fixture
                .task_mut()
                .stacks
                .visit_captured_contexts_mut(first, |context| {
                    context.local_slots.push(value);
                })
                .unwrap();
        }
    };
    gaps(&mut fixture, 32);
    let second = fixture.managed_capture("moving downstream heap string", Reg(6));
    gaps(&mut fixture, 16);
    fixture.edge(first, Reg(6));
    fixture.task_mut().registers.set(Reg(6), TaggedValue::UNIT);
    fixture.task_mut().registers.set(Reg(20), TaggedValue::UNIT);

    let addresses = |fixture: &mut Fixture| {
        let mut owner = None;
        let mut string = None;
        fixture
            .task_mut()
            .stacks
            .visit_captured_contexts_mut(first, |context| {
                let value = context.registers.get(Reg(5));
                let pointer = value.as_heap_ptr().unwrap();
                owner = Some(pointer as usize);
                // SAFETY: this is the live one-word Continuation reached through
                // the rooted first template, read under the fixture operation.
                let payload = TaggedValue::from_raw(unsafe { pointer.cast::<u64>().read() });
                assert_eq!(ContinuationId::from_u64(payload.as_u64().unwrap()), second);
            })
            .unwrap();
        fixture
            .task_mut()
            .stacks
            .visit_captured_contexts_mut(second, |context| {
                string = Some(context.registers.get(Reg(10)).as_heap_ptr().unwrap() as usize);
            })
            .unwrap();
        (owner.unwrap(), string.unwrap())
    };
    let before = addresses(&mut fixture);
    let mut moved_owner = false;
    let mut moved_string = false;
    for _ in 0..6 {
        fixture.collect();
        fixture.assert_counts(2, 3);
        fixture.assert_payload(first, "moving chain first", 1);
        fixture.assert_payload(second, "moving downstream heap string", 1);
        assert_eq!(fixture.owner_handle(Reg(4)), first);
        let after = addresses(&mut fixture);
        moved_owner |= after.0 != before.0;
        moved_string |= after.1 != before.1;
    }
    assert!(
        moved_owner,
        "weak-only downstream continuation wrapper must actually move"
    );
    assert!(
        moved_string,
        "downstream captured heap string must actually move"
    );
    fixture.clear_ordinary_roots();
    fixture.collect();
    fixture.assert_counts(0, 1);
}
