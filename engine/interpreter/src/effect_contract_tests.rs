//! Language continuation inputs are checked before changing capture ownership.

use nsbc::{FuncId, Instruction, Reg};
use runtime::{
    ContinuationId, EffectHandler, FunctionCode, PromptId, StackError, TaggedValue, TaskId,
};
use type_pool::{Intrinsic, ScopeContext, TraitImplRecord, TypeIndex, TypeKind, TypePool};

use crate::{DispatchResult, Vm, VmError, builtin_ctx::heap_string_to_owned, tests::make_vm};

fn task(vm: &mut Vm) -> TaskId {
    vm.spawn_root(FuncId(0))
}

fn capture(vm: &mut Vm, task: TaskId, input: TypeIndex) -> (ContinuationId, TaggedValue) {
    let stacks = &mut vm.roots.scheduler.get_task_mut(task).unwrap().stacks;
    stacks.enter_delimiter(PromptId(7), FuncId(0), &[]).unwrap();
    let handle = stacks.capture(PromptId(7), Reg(3)).unwrap();
    let owner = vm.alloc_effect_continuation(task, handle, input).unwrap();
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(4), owner);
    (handle, owner)
}

fn counts(vm: &Vm, task: TaskId) -> (usize, usize) {
    (
        vm.roots
            .scheduler
            .get_task(task)
            .unwrap()
            .stacks
            .captured_count(),
        vm.roots.scheduler.stack_pool().active_count(),
    )
}

fn invalid_input(result: DispatchResult) {
    assert!(
        matches!(result, DispatchResult::Error(VmError::TypeError)),
        "expected input TypeError"
    );
}

fn invalid_capture(result: DispatchResult) {
    assert!(
        matches!(
            result,
            DispatchResult::Error(VmError::Stack(StackError::InvalidContinuation))
        ),
        "expected invalid continuation ownership"
    );
}

#[test]
fn rejected_multishot_input_does_not_allocate_or_change_capture() {
    let mut vm = make_vm();
    let task = task(&mut vm);
    let (handle, owner) = capture(&mut vm, task, Intrinsic::I64.type_index());
    let before = counts(&vm, task);
    let mut address = None;
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .stacks
        .visit_captured_contexts_mut(handle, |context| address = Some(context as *mut _ as usize))
        .unwrap();
    invalid_input(vm.resume_language_value(task, owner, TaggedValue::TRUE));
    assert_eq!(counts(&vm, task), before);
    assert!(vm.roots.continuations.contains_key(&handle));
    assert!(matches!(
        vm.resume_language_value(task, owner, TaggedValue::from_i64(42)),
        DispatchResult::Continue
    ));
    assert_eq!(counts(&vm, task), (1, before.1 + 1));
    let stacks = &mut vm.roots.scheduler.get_task_mut(task).unwrap().stacks;
    assert_ne!(stacks.active() as *const _ as usize, address.unwrap());
    assert_eq!(stacks.active().registers.get(Reg(3)).as_i64(), Some(42));
    assert!(stacks.return_from_delimiter(TaggedValue::UNIT));
    assert_eq!(counts(&vm, task), before);
}

#[test]
fn rejected_once_input_preserves_template_then_valid_once_consumes_it() {
    let mut vm = make_vm();
    let task = task(&mut vm);
    let (handle, _) = capture(&mut vm, task, Intrinsic::I64.type_index());
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(1), TaggedValue::TRUE);
    let instruction = Instruction::resume_continuation_once(Reg(4), Reg(1));
    let before = counts(&vm, task);
    invalid_input(vm.dispatch_language_resume(task, &instruction));
    assert_eq!(counts(&vm, task), before);
    assert!(vm.roots.continuations.contains_key(&handle));
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(1), TaggedValue::from_i64(42));
    assert!(matches!(
        vm.dispatch_language_resume(task, &instruction),
        DispatchResult::Continue
    ));
    assert_eq!(counts(&vm, task), (0, before.1));
    assert!(!vm.roots.continuations.contains_key(&handle));
    assert_eq!(
        vm.roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(Reg(3))
            .as_i64(),
        Some(42)
    );
}

#[test]
fn clone_preserves_input_contract_and_invalid_handle_never_bypasses_it() {
    let mut vm = make_vm();
    let task = task(&mut vm);
    let (_, owner) = capture(&mut vm, task, Intrinsic::Unit.type_index());
    let foreign = vm.spawn_root(FuncId(0));
    invalid_capture(vm.resume_language_value(foreign, owner, TaggedValue::UNIT));
    invalid_capture(vm.clone_language_continuation(foreign, owner));
    assert!(matches!(
        vm.clone_language_continuation(task, owner),
        DispatchResult::Continue
    ));
    let clone = vm
        .roots
        .scheduler
        .get_task(task)
        .unwrap()
        .registers
        .get(Reg(0));
    let before = counts(&vm, task);
    invalid_input(vm.resume_language_value(task, clone, TaggedValue::TRUE));
    assert_eq!(counts(&vm, task), before);
    assert!(matches!(
        vm.resume_language_value(task, clone, TaggedValue::UNIT),
        DispatchResult::Continue
    ));
    assert!(
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .stacks
            .return_from_delimiter(TaggedValue::UNIT)
    );
    let unknown = ContinuationId::from_u64((1 << 57) - 1);
    let stacks = &mut vm.roots.scheduler.get_task_mut(task).unwrap().stacks;
    stacks.enter_delimiter(PromptId(8), FuncId(0), &[]).unwrap();
    let raw = stacks.capture(PromptId(8), Reg(3)).unwrap();
    // An initialized wrapper without managed ownership must not grant raw access.
    let raw_owner = vm
        .alloc_effect_continuation(task, raw, Intrinsic::Any.type_index())
        .unwrap();
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(5), raw_owner);
    vm.roots.continuations.remove(&raw);
    invalid_capture(vm.resume_language_value(task, raw_owner, TaggedValue::UNIT));
    invalid_capture(vm.clone_language_continuation(task, raw_owner));
    assert!(
        vm.roots
            .scheduler
            .get_task(task)
            .unwrap()
            .stacks
            .has_capture(raw)
    );
    assert!(
        !vm.roots
            .scheduler
            .get_task(task)
            .unwrap()
            .stacks
            .has_capture(unknown)
    );
}

#[test]
fn heap_null_and_unit_inputs_survive_completed_collections_and_resume() {
    let mut vm = make_vm();
    let task = task(&mut vm);
    let (_, owner) = capture(&mut vm, task, Intrinsic::Str.type_index());
    let tuple_type = vm.state.type_pool.intern_structural(TypeKind::Tuple {
        elements: vec![Intrinsic::Continuation.type_index()],
    });
    // SAFETY: TestVm holds an operation; initialize and publish before another allocation.
    let pointer = unsafe { vm.state.heap.alloc_object(tuple_type, 1) }.unwrap();
    unsafe {
        pointer.as_ptr().cast::<u64>().write(owner.raw());
    }
    let holder = unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) };
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(5), holder);
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(4), TaggedValue::UNIT);
    let text = vm
        .alloc_string("typed continuation string survives collection")
        .unwrap();
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(1), text);
    let before = vm.completed_collections();
    assert!(vm.collect_garbage().unwrap());
    assert!(vm.completed_collections() > before);
    let task_state = vm.roots.scheduler.get_task(task).unwrap();
    let holder = task_state.registers.get(Reg(5));
    let value = task_state.registers.get(Reg(1));
    let owner = vm.tuple_values(holder).unwrap()[0];
    assert!(matches!(
        vm.resume_language_value(task, owner, value),
        DispatchResult::Continue
    ));
    assert_eq!(
        heap_string_to_owned(
            vm.roots
                .scheduler
                .get_task(task)
                .unwrap()
                .registers
                .get(Reg(3))
        )
        .as_deref(),
        Some("typed continuation string survives collection")
    );
    assert!(
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .stacks
            .return_from_delimiter(TaggedValue::UNIT)
    );
    let optional = vm.state.type_pool.intern_structural(TypeKind::Optional {
        inner: Intrinsic::Str.type_index(),
    });
    let (_, owner) = capture(&mut vm, task, optional);
    assert!(matches!(
        vm.resume_language_value(task, owner, TaggedValue::NULL),
        DispatchResult::Continue
    ));
    assert!(
        vm.roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(Reg(3))
            .is_null()
    );
    assert!(
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .stacks
            .return_from_delimiter(TaggedValue::UNIT)
    );
    let (_, owner) = capture(&mut vm, task, Intrinsic::Unit.type_index());
    assert!(matches!(
        vm.resume_language_value(task, owner, TaggedValue::UNIT),
        DispatchResult::Continue
    ));
    assert!(
        vm.roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(Reg(3))
            .is_unit()
    );
}

#[test]
fn numeric_resume_preserves_declared_width_including_conversion_allocation() {
    let mut vm = make_vm();
    let task = task(&mut vm);
    let (_, owner) = capture(&mut vm, task, Intrinsic::I32.type_index());
    let narrow = vm
        .cast_value(TaggedValue::from_i64(42), Intrinsic::I8.type_index(), 0)
        .unwrap();
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .registers
        .set(Reg(1), narrow);
    let allocations = vm.state.heap.alloc_count();
    assert!(matches!(
        vm.resume_language_value(task, owner, narrow),
        DispatchResult::Continue
    ));
    let value = vm
        .roots
        .scheduler
        .get_task(task)
        .unwrap()
        .registers
        .get(Reg(3));
    assert_eq!(
        vm.reflected_type(value).unwrap(),
        Intrinsic::I32.type_index()
    );
    assert!(vm.state.heap.alloc_count() > allocations);
}

#[test]
fn frozen_trait_scope_is_restored_on_success_failure_and_clone() {
    let mut vm = make_vm();
    let mut pool = TypePool::with_intrinsics();
    pool.install_scopes(vec![
        ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        },
        ScopeContext {
            parent: Some(0),
            package: 0,
            assoc_type: None,
        },
    ])
    .unwrap();
    let marker = pool.well_known.eq;
    pool.add_trait_impl(TraitImplRecord {
        implementor: Intrinsic::I64.type_index(),
        trait_type: marker,
        visible_scope: Some(1),
        methods: vec![],
    });
    vm.install_type_pool(pool);
    let task = task(&mut vm);
    vm.state.type_query_scope = Some(1);
    let (_, owner) = capture(&mut vm, task, marker);
    vm.state.type_query_scope = Some(0);
    invalid_input(vm.resume_language_value(task, owner, TaggedValue::TRUE));
    assert_eq!(vm.state.type_query_scope, Some(0));
    assert!(matches!(
        vm.clone_language_continuation(task, owner),
        DispatchResult::Continue
    ));
    let clone = vm
        .roots
        .scheduler
        .get_task(task)
        .unwrap()
        .registers
        .get(Reg(0));
    assert!(matches!(
        vm.resume_language_value(task, clone, TaggedValue::from_i64(42)),
        DispatchResult::Continue
    ));
    assert_eq!(vm.state.type_query_scope, Some(0));
    assert_eq!(
        vm.roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(Reg(3))
            .as_i64(),
        Some(42)
    );
    assert!(
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .stacks
            .return_from_delimiter(TaggedValue::UNIT)
    );
    vm.state.type_query_scope = None;
    let (_, unscoped) = capture(&mut vm, task, marker);
    vm.state.type_query_scope = Some(1);
    assert!(matches!(
        vm.resume_language_value(task, unscoped, TaggedValue::from_i64(42)),
        DispatchResult::Error(VmError::MissingTraitContext)
    ));
    assert_eq!(vm.state.type_query_scope, Some(1));
}

#[test]
fn effect_dispatch_derives_resume_input_from_declared_effect_metadata() {
    let mut vm = make_vm();
    let effect = vm.state.type_pool.intern_structural(TypeKind::Effect {
        params: vec![],
        ret: Intrinsic::I64.type_index(),
        is_async: false,
    });
    vm.add_function(FunctionCode {
        display_owner: None,
        abi: None,
        func_id: FuncId(0),
        instructions: vec![],
        register_count: 32,
        param_count: 1,
        is_closure: false,
        function_type: TypeIndex::INVALID,
    });
    let task = task(&mut vm);
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .handler_stack
        .push(EffectHandler {
            effect_type: effect,
            handler_func: FuncId(0),
            is_async: false,
            closure_env: None,
            continuation_param: Some(0),
        });
    vm.roots
        .scheduler
        .get_task_mut(task)
        .unwrap()
        .stacks
        .enter_delimiter(PromptId(effect.as_u32()), FuncId(0), &[])
        .unwrap();
    assert!(matches!(
        vm.dispatch_effect(task, &Instruction::effect_call(effect, 0)),
        DispatchResult::Continue
    ));
    let owner = vm
        .roots
        .scheduler
        .get_task(task)
        .unwrap()
        .registers
        .get(Reg(0));
    let before = counts(&vm, task);
    invalid_input(vm.resume_language_value(task, owner, TaggedValue::TRUE));
    assert_eq!(counts(&vm, task), before);
}
