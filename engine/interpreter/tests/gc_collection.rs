//! Real collections in a separate test executable, with a controlled heap/plan.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use gc::GcConfig;
use interpreter::{BuiltinCtx, Vm, VmError, VmResult};
use nsbc::{AddrMode, Constant, FuncId, GlobalInfo, Instruction, Opcode, Reg};
use runtime::{FunctionCode, Number};
use stack_pool::StackPool;
use type_pool::{Intrinsic, TypePool};

fn vm() -> Vm {
    Vm::new(
        TypePool::with_intrinsics(),
        &GcConfig {
            heap_size: 32 * 1024 * 1024,
            plan: "Immix".into(),
            gc_threads: 1,
        },
        Arc::new(StackPool::new()),
    )
}

fn add_function(vm: &mut Vm, id: u32, instructions: &[Instruction], params: u8) {
    add_function_with_abi(vm, id, instructions, params, None);
}

fn add_function_with_abi(
    vm: &mut Vm,
    id: u32,
    instructions: &[Instruction],
    params: u8,
    abi: Option<nsbc::FunctionAbi>,
) {
    vm.add_function(FunctionCode {
        display_owner: None,
        abi,
        func_id: FuncId(id),
        instructions: instructions
            .iter()
            .map(|instruction| instruction.encode())
            .collect(),
        register_count: 32,
        param_count: params,
        is_closure: params == 8,
        function_type: type_pool::TypeIndex::INVALID,
    });
}

fn make_string(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_string("abcdefgh")
}

fn seed_growing_map(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    // One Unit argument permits reading each freshly published string through
    // arg(0), which roots it across subsequent allocations in this native call.
    ctx.require_arity(1)?;
    ctx.return_empty_map()?;
    let map = ctx.arg(0)?;
    ctx.return_string("only-key")?;
    let key = ctx.arg(0)?;
    ctx.return_string("abcdefgh")?;
    let value = ctx.arg(0)?;
    ctx.map_set(map, key, value)?;
    for index in 0..128 {
        ctx.return_string(&format!("extra{index}"))?;
        let key = ctx.arg(0)?;
        ctx.map_set(map, key, runtime::TaggedValue::UNIT)?;
    }
    ctx.set_return(map)
}

fn collect_and_check_map(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let map = ctx.arg(0)?;
    ctx.return_unit();
    // The previous native context has exited. Only this Map graph owns every
    // dynamically allocated key and its string value after buffer growth.
    assert!(ctx.collect_garbage()?);
    ctx.return_string("only-key")?;
    let key = ctx.arg(0)?;
    assert!(ctx.map_contains(map, key)?);
    let value = ctx.map_get(map, key)?;
    assert_eq!(ctx.format_value(value)?, "abcdefgh");
    assert_eq!(ctx.map_len(map)?, 129);
    ctx.return_i64(8)
}

fn remove_map_value_and_collect(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let map = ctx.arg(0)?;
    ctx.return_string("only-key")?;
    let key = ctx.arg(0)?;
    let removed = ctx.map_remove(map, key)?;
    assert!(!ctx.map_contains(map, key)?);
    ctx.return_unit();
    // The removed value has no bucket/register root: map_remove's native
    // result root must survive this collection before publication to r0.
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(removed)?, "abcdefgh");
    ctx.set_return(removed)
}

#[test]
fn collection_preserves_keys_and_values_owned_only_by_a_growing_map() {
    let mut engine = vm();
    engine.register_builtin(0, seed_growing_map);
    engine.register_builtin(1, collect_and_check_map);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::load_unit(Reg(0)),
            Instruction::call_builtin(0, 1),
            Instruction::call_builtin(1, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn removed_map_value_survives_context_roots_and_return_register_publication() {
    let mut engine = vm();
    engine.register_builtin(0, seed_growing_map);
    engine.register_builtin(1, remove_map_value_and_collect);
    engine.register_builtin(2, retain_argument);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::load_unit(Reg(0)),
            Instruction::call_builtin(0, 1),
            Instruction::call_builtin(1, 1),
            // A second collection uses the value after the removing Ctx drops.
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn collection_preserves_maps_in_detached_continuation_segments() {
    let mut engine = vm();
    engine.push_constant(&Constant::Str("key".into())).unwrap();
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::load_imm(Reg(20), 7),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::mov(Reg(21), Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::load_imm(Reg(1), 40),
            Instruction::resume(Reg(21), Reg(1)),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    add_function(
        &mut engine,
        1,
        &[
            Instruction::allocate_slots(1),
            Instruction::a_type(Opcode::NewMap, AddrMode::Imm, Reg(10), Reg(0), 0),
            Instruction::call_builtin(0, 0),
            Instruction::load_const(Reg(1), 0),
            Instruction::a_type(Opcode::StoreIndex, AddrMode::Imm, Reg(0), Reg(10), 1),
            Instruction::store_slot(0, Reg(10)),
            Instruction::load_unit(Reg(10)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_imm(Reg(20), 7),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::load_slot(Reg(10), 0),
            Instruction::load_const(Reg(1), 0),
            Instruction::a_type(Opcode::LoadIndex, AddrMode::Imm, Reg(0), Reg(10), 1),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

fn collect(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    assert!(ctx.collect_garbage()?);
    ctx.return_unit();
    Ok(())
}

fn length(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let length = ctx.arg_string(0)?.len();
    ctx.return_i64(length as i64)?;
    Ok(())
}

fn retain_argument(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let value = ctx.arg(0)?;
    ctx.return_unit(); // the argument register no longer keeps the value alive
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(value)?, "abcdefgh");
    ctx.return_i64(8)?;
    Ok(())
}

fn retain_numeric_argument(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg(0)?;
    let number = ctx.arg_number(0)?;
    ctx.return_unit();
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(value)?, number.to_string());
    ctx.return_number(number)
}

fn retain_type_argument(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg(0)?;
    assert_eq!(ctx.arg_type(0)?, Intrinsic::I64.type_index());
    ctx.return_unit();
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(value)?, "i64");
    ctx.return_i64(42)
}

#[test]
fn collection_preserves_a_heap_value_rooted_only_in_globals() {
    let mut engine = vm();
    engine
        .initialize_globals(&[GlobalInfo {
            type_index: Intrinsic::Str.type_index(),
            is_mutable: false,
        }])
        .unwrap();
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::call_builtin(0, 0),
            Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(0), 0),
            // Neither registers nor constants retain the allocated string.
            Instruction::load_unit(Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::a_type(Opcode::LoadGlobal, AddrMode::Imm, Reg(0), Reg(0), 0),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let before = engine.completed_collections();
    let task = engine.spawn_root(FuncId(0));
    let result = engine.run();
    assert!(matches!(result, VmResult::Finished), "{result:?}");
    assert!(engine.completed_collections() > before);
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn collection_preserves_a_delimited_entry_closures_captured_heap_value() {
    let mut engine = vm();
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::call_builtin(0, 0),
            Instruction::new_closure(Reg(1), 1, 1),
            Instruction::reset_closure(Reg(1), 0),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    engine.add_function(FunctionCode {
        display_owner: None,
        abi: Some(nsbc::FunctionAbi {
            captures: vec![nsbc::CaptureAbi::Value],
            parameters: vec![],
        }),
        func_id: FuncId(1),
        instructions: [
            Instruction::load_unit(Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::load_capture(Reg(0), 0),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ]
        .iter()
        .map(|instruction| instruction.encode())
        .collect(),
        register_count: 32,
        param_count: 1,
        is_closure: true,
        function_type: type_pool::TypeIndex::INVALID,
    });
    let before = engine.completed_collections();
    let task = engine.spawn_root(FuncId(0));
    let result = engine.run();
    assert!(matches!(result, VmResult::Finished), "{result:?}");
    assert!(engine.completed_collections() > before);
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn collection_preserves_immediate_type_descriptors_in_slots_and_native_values() {
    let mut engine = vm();
    engine.register_builtin(0, retain_type_argument);
    engine.register_builtin(1, collect);
    let index = engine
        .push_constant(&Constant::Type(Intrinsic::I64.type_index()))
        .unwrap();
    add_function(
        &mut engine,
        0,
        &[
            Instruction::allocate_slots(1),
            Instruction::load_const(Reg(0), index as u16),
            Instruction::store_slot(0, Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::load_slot(Reg(0), 0),
            Instruction::call_builtin(0, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    let result = engine.run();
    assert!(matches!(result, VmResult::Finished), "{result:?}");
    assert_eq!(engine.task_result_i64(task).unwrap(), 42);
}

#[test]
fn collection_preserves_exact_boxed_numeric_payloads() {
    let mut engine = vm();
    engine.register_builtin(0, retain_numeric_argument);
    let tiny_subnormal = f64::from_bits(1);
    let nan = f64::from_bits(0x7ff8_0000_0000_0042);
    let fixtures = [
        (Constant::Int(i64::MAX), Number::I64(i64::MAX)),
        (Constant::Int(i64::MIN), Number::I64(i64::MIN)),
        (Constant::UInt(u64::MAX), Number::U64(u64::MAX)),
        (Constant::Int128(i128::MIN), Number::I128(i128::MIN)),
        (Constant::Int128(i128::MAX), Number::I128(i128::MAX)),
        (Constant::UInt128(u128::MAX), Number::U128(u128::MAX)),
        // Low scalar bits look like aligned heap pointers; scanning the payload
        // as TaggedValues could enqueue invalid references such as address 8.
        (
            Constant::UInt128((8_u128 << 64) | 16),
            Number::U128((8_u128 << 64) | 16),
        ),
        (Constant::Int128(42), Number::I128(42)),
        (Constant::Float(0.1), Number::F64(0.1)),
        (Constant::Float(tiny_subnormal), Number::F64(tiny_subnormal)),
        (Constant::Float(-0.0), Number::F64(-0.0)),
        (Constant::Float(nan), Number::F64(nan)),
    ];
    let narrow_function_base = fixtures.len() as u32;
    for (index, (constant, expected)) in fixtures.into_iter().enumerate() {
        let constant_id = engine.push_constant(&constant).unwrap();
        add_function(
            &mut engine,
            index as u32,
            &[
                Instruction::load_const(Reg(0), constant_id as u16),
                Instruction::call_builtin(0, 1),
                Instruction::ret(Reg(0)),
            ],
            0,
        );
        let task = engine.spawn_root(FuncId(index as u32));
        assert!(matches!(engine.run(), VmResult::Finished));
        let actual = engine.task_result_number(task).unwrap();
        match (actual, expected) {
            (Number::F64(actual), Number::F64(expected)) => {
                assert_eq!(actual.to_bits(), expected.to_bits())
            }
            (actual, expected) => assert_eq!(actual, expected),
        }
        assert_eq!(engine.active_stack_count(), 0);
    }
    for (index, target) in [
        Intrinsic::I8,
        Intrinsic::U8,
        Intrinsic::I16,
        Intrinsic::U16,
        Intrinsic::I32,
        Intrinsic::U32,
        Intrinsic::Isize,
        Intrinsic::Usize,
        Intrinsic::F32,
    ]
    .into_iter()
    .enumerate()
    {
        let input = if target == Intrinsic::F32 {
            Number::F64(0.1)
        } else {
            Number::I64(42)
        };
        let constant = match input {
            Number::F64(value) => Constant::Float(value),
            _ => Constant::Int(42),
        };
        let constant_id = engine.push_constant(&constant).unwrap();
        let function_id = narrow_function_base + index as u32;
        add_function(
            &mut engine,
            function_id,
            &[
                Instruction::load_const(Reg(0), constant_id as u16),
                Instruction::a_type(
                    Opcode::TypeCast,
                    AddrMode::Imm,
                    Reg(0),
                    Reg(0),
                    target.type_index().as_u32() as u16,
                ),
                Instruction::call_builtin(0, 1),
                Instruction::ret(Reg(0)),
            ],
            0,
        );
        let task = engine.spawn_root(FuncId(function_id));
        assert!(matches!(engine.run(), VmResult::Finished));
        assert_eq!(
            engine.task_result_number(task).unwrap(),
            input.cast_to(target).unwrap()
        );
    }
}

#[test]
fn collection_preserves_idle_engine_and_captured_frame_roots() {
    let mut idle = vm();
    let small = idle
        .push_constant(&Constant::Str("idledata".into()))
        .unwrap();
    let large_value = "a".repeat(70_000);
    let large = idle
        .push_constant(&Constant::Str(large_value.clone()))
        .unwrap();
    // Moving the engine must not change its registered root address.
    let mut idle = Box::new(idle);
    let mut active = vm();
    active.register_builtin(0, make_string);
    active.register_builtin(1, collect);
    active.register_builtin(2, length);
    add_function(
        &mut active,
        0,
        &[
            Instruction::load_imm(Reg(20), 7),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::mov(Reg(21), Reg(0)),
            Instruction::call_builtin(1, 0), // collect while the delimiter is detached
            Instruction::load_imm(Reg(1), 40),
            Instruction::resume(Reg(21), Reg(1)),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    add_function(
        &mut active,
        1,
        &[
            Instruction::allocate_slots(1),
            Instruction::call_builtin(0, 0),
            Instruction::store_slot(0, Reg(0)),
            Instruction::call_builtin(0, 0), // a distinct string owned only by captures
            // Raw function/count words both equal 8, and must not be traced as refs.
            Instruction::new_closure(Reg(23), 8, 8),
            Instruction::call(2, 0),
            Instruction::mov(Reg(22), Reg(0)),
            Instruction::load_slot(Reg(0), 0),
            Instruction::call_builtin(2, 1),
            Instruction::add(Reg(22), Reg(22), Reg(0)),
            Instruction::call_indirect(Reg(23), 0),
            Instruction::add(Reg(0), Reg(0), Reg(22)),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    add_function(
        &mut active,
        2,
        &[
            Instruction::allocate_slots(1),
            Instruction::call_builtin(0, 0),
            Instruction::store_slot(0, Reg(0)),
            Instruction::load_imm(Reg(20), 7),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::mov(Reg(22), Reg(0)),
            Instruction::load_slot(Reg(0), 0),
            Instruction::call_builtin(2, 1),
            Instruction::add(Reg(0), Reg(0), Reg(22)),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    for id in 3..8 {
        add_function(&mut active, id, &[Instruction::return_unit()], 0);
    }
    add_function_with_abi(
        &mut active,
        8,
        &[Instruction::call_builtin(2, 1), Instruction::ret(Reg(0))],
        8,
        Some(nsbc::FunctionAbi {
            captures: vec![nsbc::CaptureAbi::Value; 8],
            parameters: vec![],
        }),
    );
    let before = active.completed_collections();
    let task = active.spawn_root(FuncId(0));
    assert!(matches!(active.run(), VmResult::Finished));
    assert!(active.completed_collections() > before);
    assert_eq!(active.task_result_i64(task).unwrap(), 64);
    assert_eq!(active.active_stack_count(), 0);
    assert_eq!(idle.constant_string(small).unwrap(), "idledata");
    assert_eq!(idle.constant_string(large).unwrap(), large_value);
    for _ in 0..3 {
        assert!(active.collect_garbage().unwrap());
    }
    assert_eq!(idle.constant_string(large).unwrap(), large_value);
    active.register_builtin(3, retain_argument);
    add_function(
        &mut active,
        9,
        &[
            Instruction::call_builtin(0, 0),
            Instruction::call_builtin(3, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let argument_task = active.spawn_root(FuncId(9));
    assert!(matches!(active.run(), VmResult::Finished));
    assert_eq!(active.task_result_i64(argument_task).unwrap(), 8);
    drop(active);
    assert!(idle.collect_garbage().unwrap());
    assert_eq!(idle.constant_string(small).unwrap(), "idledata");
    assert!(matches!(
        idle.push_constant(&Constant::Str("a".repeat(524_281))),
        Err(VmError::ObjectTooLarge)
    ));
    assert!(matches!(
        idle.push_constant(&Constant::BigInt("1".into())),
        Err(VmError::UnsupportedConstant)
    ));
}

thread_local! {
    static OTHER_VM: RefCell<Option<Vm>> = const { RefCell::new(None) };
}

fn nested_execution(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    OTHER_VM.with(|other| {
        let mut other = other.borrow_mut();
        let other = other.as_mut().unwrap();
        assert!(matches!(
            other.collect_garbage(),
            Err(VmError::CrossVmOperation)
        ));
        assert!(matches!(
            other.push_constant(&Constant::Str("nested".into())),
            Err(VmError::CrossVmOperation)
        ));
        assert!(matches!(
            other.run(),
            VmResult::Error(VmError::CrossVmOperation)
        ));
    });
    ctx.return_i64(42)?;
    Ok(())
}

#[test]
fn cross_vm_execution_returns_an_error_instead_of_waiting_for_its_caller() {
    OTHER_VM.with(|other| *other.borrow_mut() = Some(vm()));
    let mut engine = vm();
    engine.register_builtin(0, nested_execution);
    add_function(
        &mut engine,
        0,
        &[Instruction::call_builtin(0, 0), Instruction::ret(Reg(0))],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 42);
    OTHER_VM.with(|other| {
        other.borrow_mut().take();
    });
}

static READY: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
static FINISH: AtomicBool = AtomicBool::new(false);

fn loop_condition(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    *READY.0.lock().unwrap() = true;
    READY.1.notify_all();
    ctx.return_bool(FINISH.load(Ordering::Acquire));
    Ok(())
}

#[test]
fn nonallocating_vm_loop_stops_for_another_engine_collection() {
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut engine = vm();
        let index = engine
            .push_constant(&Constant::Str("threaded".into()))
            .unwrap();
        engine.register_builtin(0, loop_condition);
        add_function(
            &mut engine,
            0,
            &[
                Instruction::call_builtin(0, 0),
                Instruction::jmp_if_not(Reg(0), -1),
                Instruction::load_imm(Reg(0), 42),
                Instruction::ret(Reg(0)),
            ],
            0,
        );
        let task = engine.spawn_root(FuncId(0));
        assert!(matches!(engine.run(), VmResult::Finished));
        assert_eq!(engine.task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.constant_string(index).unwrap(), "threaded");
        finished_tx.send(()).unwrap();
    });
    let mut ready = READY.0.lock().unwrap();
    while !*ready {
        let (next, timeout) = READY.1.wait_timeout(ready, Duration::from_secs(5)).unwrap();
        ready = next;
        assert!(!timeout.timed_out());
    }
    drop(ready);
    let mut collector = vm();
    let before = collector.completed_collections();
    for _ in 0..3 {
        assert!(collector.collect_garbage().unwrap());
    }
    assert!(collector.completed_collections() >= before + 3);
    FINISH.store(true, Ordering::Release);
    finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.join().unwrap();
}

#[test]
fn collection_preserves_a_string_rooted_only_in_a_struct_field() {
    let mut pool = TypePool::with_intrinsics();
    let ty = pool.register(type_pool::TypeInfo {
        kind: type_pool::TypeKind::Struct {
            name: str_interner::intern("TextBox"),
            fields: vec![type_pool::FieldInfo {
                name: str_interner::intern("text"),
                ty: Intrinsic::Str.type_index(),
                has_default: false,
                offset: 0,
            }],
        },
        type_id: type_pool::TypeId(98, 100),
        size: 8,
        align: 8,
    });
    let mut engine = vm();
    engine.install_type_pool(pool);
    engine
        .initialize_globals(&[GlobalInfo {
            type_index: ty,
            is_mutable: false,
        }])
        .unwrap();
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::new_object(Reg(20), ty),
            Instruction::call_builtin(0, 0),
            Instruction::store_field(Reg(20), 0, Reg(0)),
            Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(20), 0),
            Instruction::load_unit(Reg(20)),
            Instruction::load_unit(Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::a_type(Opcode::LoadGlobal, AddrMode::Imm, Reg(20), Reg(0), 0),
            Instruction::load_field(Reg(0), Reg(20), 0),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let before = engine.completed_collections();
    let task = engine.spawn_root(FuncId(0));
    let result = engine.run();
    assert!(matches!(result, VmResult::Finished), "{result:?}");
    assert!(engine.completed_collections() > before);
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

fn empty_list(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_empty_list()
}

fn append_list(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let list = ctx.arg(0)?;
    let value = ctx.arg(1)?;
    ctx.list_push(list, value)?;
    ctx.set_return(list)
}

fn grow_and_collect_list(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg(0)?;
    ctx.return_unit();
    // Only the List buffer retains the string; growing replaces that buffer.
    for _ in 0..128 {
        ctx.list_push(list, runtime::TaggedValue::UNIT)?;
    }
    assert!(ctx.collect_garbage()?);
    let string = ctx.list_get(list, runtime::TaggedValue::from_u64(0))?;
    assert_eq!(ctx.format_value(string)?, "abcdefgh");
    assert_eq!(ctx.list_len(list)?, 129);
    ctx.return_i64(8)
}

#[test]
fn collection_preserves_strings_owned_only_by_a_growing_list() {
    let mut engine = vm();
    engine.register_builtin(0, empty_list);
    engine.register_builtin(1, make_string);
    engine.register_builtin(2, append_list);
    engine.register_builtin(3, grow_and_collect_list);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::call_builtin(0, 0),
            Instruction::mov(Reg(10), Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::mov(Reg(1), Reg(0)),
            Instruction::mov(Reg(0), Reg(10)),
            Instruction::call_builtin(2, 2),
            Instruction::load_unit(Reg(1)),
            Instruction::load_unit(Reg(10)),
            Instruction::call_builtin(3, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn collection_preserves_lists_in_detached_continuation_segments() {
    let mut engine = vm();
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::load_imm(Reg(20), 7),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::mov(Reg(21), Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::load_imm(Reg(1), 40),
            Instruction::resume(Reg(21), Reg(1)),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    add_function(
        &mut engine,
        1,
        &[
            Instruction::allocate_slots(1),
            Instruction::a_type(Opcode::NewList, AddrMode::Imm, Reg(10), Reg(0), 1),
            Instruction::call_builtin(0, 0),
            Instruction::load_imm(Reg(1), 0),
            Instruction::a_type(Opcode::StoreIndex, AddrMode::Imm, Reg(0), Reg(10), 1),
            Instruction::store_slot(0, Reg(10)),
            Instruction::load_unit(Reg(10)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_imm(Reg(20), 7),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::load_slot(Reg(10), 0),
            Instruction::load_imm(Reg(1), 0),
            Instruction::a_type(Opcode::LoadIndex, AddrMode::Imm, Reg(0), Reg(10), 1),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

fn pop_and_collect_list_element(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg(0)?;
    let element = ctx.list_pop(list)?;
    assert_eq!(ctx.list_len(list)?, 0);
    ctx.return_unit();
    // Neither the cleared register nor the now-empty List buffer owns the string.
    // The native context must keep the popped result rooted until publication.
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(element)?, "abcdefgh");
    ctx.set_return(element)
}

#[test]
fn collection_preserves_popped_heap_elements_before_and_after_return_publication() {
    let mut engine = vm();
    engine.register_builtin(0, empty_list);
    engine.register_builtin(1, make_string);
    engine.register_builtin(2, append_list);
    engine.register_builtin(3, pop_and_collect_list_element);
    engine.register_builtin(4, retain_argument);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::call_builtin(0, 0),
            Instruction::mov(Reg(10), Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::mov(Reg(1), Reg(0)),
            Instruction::mov(Reg(0), Reg(10)),
            Instruction::call_builtin(2, 2),
            Instruction::load_unit(Reg(1)),
            Instruction::load_unit(Reg(10)),
            Instruction::call_builtin(3, 1),
            // The popped result now lives in the return register, across context drop.
            Instruction::call_builtin(4, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn collection_preserves_strings_rooted_only_in_tuple_fields() {
    let mut pool = TypePool::with_intrinsics();
    let ty = pool.intern_structural(type_pool::TypeKind::Tuple {
        elements: vec![Intrinsic::Str.type_index()],
    });
    let mut engine = vm();
    engine.install_type_pool(pool);
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::new_object(Reg(20), ty),
            Instruction::call_builtin(0, 0),
            Instruction::store_field(Reg(20), 0, Reg(0)),
            Instruction::load_unit(Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::load_field(Reg(0), Reg(20), 0),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn collection_preserves_tuple_fields_in_paused_continuation_segments() {
    let mut pool = TypePool::with_intrinsics();
    let ty = pool.intern_structural(type_pool::TypeKind::Tuple {
        elements: vec![Intrinsic::Str.type_index()],
    });
    let mut engine = vm();
    engine.install_type_pool(pool);
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::load_imm(Reg(20), 7),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::mov(Reg(21), Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::load_imm(Reg(1), 40),
            Instruction::resume(Reg(21), Reg(1)),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    add_function(
        &mut engine,
        1,
        &[
            Instruction::allocate_slots(1),
            Instruction::new_object(Reg(10), ty),
            Instruction::call_builtin(0, 0),
            Instruction::store_field(Reg(10), 0, Reg(0)),
            Instruction::store_slot(0, Reg(10)),
            Instruction::load_unit(Reg(10)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_imm(Reg(20), 7),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::load_slot(Reg(10), 0),
            Instruction::load_field(Reg(0), Reg(10), 0),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

fn enum_string_types() -> (TypePool, type_pool::TypeIndex, type_pool::TypeIndex) {
    let mut pool = TypePool::with_intrinsics();
    let ty = pool.register(type_pool::TypeInfo {
        kind: type_pool::TypeKind::Enum {
            name: str_interner::intern("Message"),
            variants: vec![
                type_pool::VariantInfo {
                    name: str_interner::intern("empty"),
                    tag: 0,
                    fields: vec![],
                },
                type_pool::VariantInfo {
                    name: str_interner::intern("text"),
                    tag: 1,
                    fields: vec![type_pool::FieldInfo {
                        name: str_interner::intern("value"),
                        ty: Intrinsic::Str.type_index(),
                        has_default: false,
                        offset: 0,
                    }],
                },
            ],
        },
        type_id: type_pool::TypeId(120, 121),
        size: 16,
        align: 8,
    });
    let arguments = pool.intern_structural(type_pool::TypeKind::Tuple {
        elements: vec![Intrinsic::Any.type_index()],
    });
    (pool, ty, arguments)
}

#[test]
fn collection_preserves_string_owned_only_by_an_enum_payload() {
    let (pool, ty, arguments) = enum_string_types();
    let mut engine = vm();
    engine.install_type_pool(pool);
    let descriptor = engine
        .push_constant(&Constant::Enum {
            type_index: ty,
            variant: 1,
        })
        .unwrap();
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::new_object(Reg(10), arguments),
            Instruction::call_builtin(0, 0),
            Instruction::store_field(Reg(10), 0, Reg(0)),
            Instruction::new_enum(Reg(20), Reg(10), descriptor as u16),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(10)),
            Instruction::call_builtin(1, 0),
            Instruction::enum_field(Reg(0), Reg(20), 0),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

#[test]
fn collection_preserves_enum_payloads_in_paused_continuation_segments() {
    let (pool, ty, arguments) = enum_string_types();
    let mut engine = vm();
    engine.install_type_pool(pool);
    let descriptor = engine
        .push_constant(&Constant::Enum {
            type_index: ty,
            variant: 1,
        })
        .unwrap();
    engine.register_builtin(0, make_string);
    engine.register_builtin(1, collect);
    engine.register_builtin(2, length);
    add_function(
        &mut engine,
        0,
        &[
            Instruction::load_imm(Reg(20), 7),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::mov(Reg(21), Reg(0)),
            Instruction::call_builtin(1, 0),
            Instruction::load_imm(Reg(1), 40),
            Instruction::resume(Reg(21), Reg(1)),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    add_function(
        &mut engine,
        1,
        &[
            Instruction::allocate_slots(1),
            Instruction::new_object(Reg(10), arguments),
            Instruction::call_builtin(0, 0),
            Instruction::store_field(Reg(10), 0, Reg(0)),
            Instruction::new_enum(Reg(11), Reg(10), descriptor as u16),
            Instruction::store_slot(0, Reg(11)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(10)),
            Instruction::load_unit(Reg(11)),
            Instruction::load_imm(Reg(20), 7),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::load_slot(Reg(11), 0),
            Instruction::enum_field(Reg(0), Reg(11), 0),
            Instruction::call_builtin(2, 1),
            Instruction::ret(Reg(0)),
        ],
        0,
    );
    let task = engine.spawn_root(FuncId(0));
    assert!(matches!(engine.run(), VmResult::Finished));
    assert_eq!(engine.task_result_i64(task).unwrap(), 8);
}

fn compare_strings_after_collection(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg(0)?;
    let right = ctx.arg(1)?;
    ctx.return_unit();
    assert!(ctx.collect_garbage()?);
    let left = ctx.format_value(left)?;
    let right = ctx.format_value(right)?;
    assert_eq!(left, "abcdefgh");
    assert_eq!(right, "abcdefgh");
    ctx.return_bool(left == right);
    Ok(())
}

fn equality_result(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_i64(if ctx.arg_bool(0)? { 42 } else { 0 })
}

#[test]
fn ordinary_eq_method_frames_retain_both_nested_payloads_through_gc_and_effect_pause() {
    for paused in [false, true] {
        let mut pool = TypePool::with_intrinsics();
        let inner = pool.register(type_pool::TypeInfo {
            kind: type_pool::TypeKind::Struct {
                name: str_interner::intern("InnerEqPayload"),
                fields: vec![type_pool::FieldInfo {
                    name: str_interner::intern("text"),
                    ty: Intrinsic::Str.type_index(),
                    offset: 0,
                    has_default: false,
                }],
            },
            type_id: type_pool::TypeId(410, 1),
            size: 8,
            align: 8,
        });
        let outer = pool.register(type_pool::TypeInfo {
            kind: type_pool::TypeKind::Struct {
                name: str_interner::intern("OuterEqPayload"),
                fields: vec![type_pool::FieldInfo {
                    name: str_interner::intern("inner"),
                    ty: inner,
                    offset: 0,
                    has_default: false,
                }],
            },
            type_id: type_pool::TypeId(410, 2),
            size: 8,
            align: 8,
        });
        let method = str_interner::intern("eq");
        pool.add_method(
            outer,
            type_pool::MethodSlot {
                name: method,
                func_id: 2,
                trait_impl: None,
                visible_scope: None,
                access: type_pool::MethodAccess::Public,
            },
        );
        let mut engine = vm();
        engine.install_type_pool(pool);
        engine.register_builtin(0, make_string);
        engine.register_builtin(1, collect);
        engine.register_builtin(2, compare_strings_after_collection);
        engine.register_builtin(3, equality_result);
        let mut main = if paused {
            vec![
                Instruction::load_imm(Reg(20), 7),
                Instruction::reset(Reg(20), 1, 0),
                Instruction::mov(Reg(21), Reg(0)),
                Instruction::call_builtin(1, 0),
                Instruction::load_imm(Reg(1), 40),
                Instruction::resume(Reg(21), Reg(1)),
            ]
        } else {
            vec![Instruction::call(1, 0)]
        };
        main.extend([Instruction::call_builtin(3, 1), Instruction::ret(Reg(0))]);
        add_function(&mut engine, 0, &main, 0);
        let mut builder = Vec::new();
        for receiver in [Reg(19), Reg(20)] {
            builder.extend([
                Instruction::new_object(Reg(10), inner),
                Instruction::call_builtin(0, 0),
                Instruction::store_field(Reg(10), 0, Reg(0)),
                Instruction::new_object(receiver, outer),
                Instruction::store_field(receiver, 0, Reg(10)),
                Instruction::load_unit(Reg(10)),
                Instruction::load_unit(Reg(0)),
            ]);
        }
        builder.extend([
            Instruction::mov(Reg(0), Reg(20)),
            Instruction::call_method(Reg(19), method.as_u32(), 1),
            Instruction::ret(Reg(0)),
        ]);
        add_function(&mut engine, 1, &builder, 0);
        let mut eq = vec![
            Instruction::allocate_slots(2),
            Instruction::store_slot(0, Reg(0)),
            Instruction::store_slot(1, Reg(1)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(1)),
            Instruction::load_unit(Reg(19)),
            Instruction::load_unit(Reg(20)),
        ];
        if paused {
            eq.extend([
                Instruction::load_imm(Reg(20), 7),
                Instruction::shift(Reg(20), Reg(0)),
            ]);
        }
        eq.extend([
            Instruction::call_builtin(1, 0),
            Instruction::load_slot(Reg(10), 0),
            Instruction::load_field(Reg(10), Reg(10), 0),
            Instruction::load_field(Reg(0), Reg(10), 0),
            Instruction::load_slot(Reg(11), 1),
            Instruction::load_field(Reg(11), Reg(11), 0),
            Instruction::load_field(Reg(1), Reg(11), 0),
            Instruction::call_builtin(2, 2),
            Instruction::ret(Reg(0)),
        ]);
        add_function(&mut engine, 2, &eq, 2);
        let task = engine.spawn_root(FuncId(0));
        assert!(
            matches!(engine.run(), VmResult::Finished),
            "paused={paused}"
        );
        assert_eq!(engine.task_result_i64(task).unwrap(), 42, "paused={paused}");
    }
}

fn concatenate_with_collections(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg(0)?;
    let right = ctx.arg(1)?;
    let mut text = ctx.arg_string(0)?;
    text.push_str(&ctx.arg_string(1)?);
    ctx.return_unit();
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(left)?, "abcdefgh");
    assert_eq!(ctx.format_value(right)?, "abcdefgh");
    ctx.return_string(&text)?;
    let result = ctx.arg(0)?;
    ctx.return_unit();
    // Native allocation first publishes to r0, then arg retains it before r0
    // is cleared. Neither a Rust string nor its pointer acts as a managed root.
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(result)?, "abcdefghabcdefgh");
    ctx.set_return(result)
}

fn concat_result_after_collection(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let result = ctx.arg(0)?;
    ctx.return_unit();
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(result)?, "abcdefghabcdefgh");
    ctx.return_i64(16)
}

#[test]
fn concat_method_frames_keep_operands_and_native_results_through_gc_and_effect_pause() {
    for pause in ["none", "operands", "result"] {
        let mut pool = TypePool::with_intrinsics();
        let wrapper = pool.register(type_pool::TypeInfo {
            kind: type_pool::TypeKind::Struct {
                name: str_interner::intern("ConcatOperand"),
                fields: vec![type_pool::FieldInfo {
                    name: str_interner::intern("text"),
                    ty: Intrinsic::Str.type_index(),
                    offset: 0,
                    has_default: false,
                }],
            },
            type_id: type_pool::TypeId(411, 1),
            size: 8,
            align: 8,
        });
        let name = str_interner::intern("concat");
        for (ty, function) in [(wrapper, 2), (Intrinsic::Str.type_index(), 3)] {
            pool.add_method(
                ty,
                type_pool::MethodSlot {
                    name,
                    func_id: function,
                    trait_impl: None,
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                },
            );
        }
        let mut engine = vm();
        engine.install_type_pool(pool);
        engine.register_builtin(0, make_string);
        engine.register_builtin(1, collect);
        engine.register_builtin(2, concatenate_with_collections);
        engine.register_builtin(3, concat_result_after_collection);
        let mut main = if pause == "none" {
            vec![Instruction::call(1, 0)]
        } else {
            vec![
                Instruction::load_imm(Reg(20), 7),
                Instruction::reset(Reg(20), 1, 0),
                Instruction::mov(Reg(21), Reg(0)),
                Instruction::call_builtin(1, 0),
                Instruction::load_imm(Reg(1), 40),
                Instruction::resume(Reg(21), Reg(1)),
            ]
        };
        main.extend([Instruction::call_builtin(3, 1), Instruction::ret(Reg(0))]);
        add_function(&mut engine, 0, &main, 0);
        let mut body = Vec::new();
        for receiver in [Reg(19), Reg(20)] {
            body.extend([
                Instruction::new_object(receiver, wrapper),
                Instruction::call_builtin(0, 0),
                Instruction::store_field(receiver, 0, Reg(0)),
                Instruction::load_unit(Reg(0)),
            ]);
        }
        body.extend([
            Instruction::mov(Reg(0), Reg(20)),
            Instruction::call_method(Reg(19), name.as_u32(), 1),
            Instruction::ret(Reg(0)),
        ]);
        add_function(&mut engine, 1, &body, 0);
        let mut method = vec![
            Instruction::allocate_slots(3),
            Instruction::store_slot(0, Reg(0)),
            Instruction::store_slot(1, Reg(1)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(1)),
            Instruction::load_unit(Reg(19)),
            Instruction::load_unit(Reg(20)),
        ];
        if pause == "operands" {
            method.extend([
                Instruction::load_imm(Reg(20), 7),
                Instruction::shift(Reg(20), Reg(0)),
            ]);
        }
        method.extend([
            Instruction::load_slot(Reg(10), 0),
            Instruction::load_field(Reg(10), Reg(10), 0),
            Instruction::load_slot(Reg(11), 1),
            Instruction::load_field(Reg(0), Reg(11), 0),
            // The custom method delegates to the registered intrinsic String
            // method through an ordinary VM frame, without Rust VM reentry.
            Instruction::call_method(Reg(10), name.as_u32(), 1),
            Instruction::store_slot(2, Reg(0)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(10)),
            Instruction::load_unit(Reg(11)),
            Instruction::store_slot(0, Reg(0)),
            Instruction::store_slot(1, Reg(0)),
        ]);
        if pause == "result" {
            method.extend([
                Instruction::load_imm(Reg(20), 7),
                Instruction::shift(Reg(20), Reg(0)),
            ]);
        }
        method.extend([Instruction::load_slot(Reg(0), 2), Instruction::ret(Reg(0))]);
        add_function(&mut engine, 2, &method, 2);
        add_function(
            &mut engine,
            3,
            &[
                Instruction::a_type(
                    Opcode::TypeAssert,
                    AddrMode::Imm,
                    Reg(0),
                    Reg(0),
                    Intrinsic::Str.type_index().as_u32() as u16,
                ),
                Instruction::a_type(
                    Opcode::TypeAssert,
                    AddrMode::Imm,
                    Reg(1),
                    Reg(1),
                    Intrinsic::Str.type_index().as_u32() as u16,
                ),
                Instruction::call_builtin(2, 2),
                Instruction::ret(Reg(0)),
            ],
            2,
        );
        let task = engine.spawn_root(FuncId(0));
        assert!(matches!(engine.run(), VmResult::Finished), "pause={pause}");
        assert_eq!(engine.task_result_i64(task).unwrap(), 16, "pause={pause}");
        assert_eq!(engine.active_stack_count(), 0);
    }
}

fn make_lookup_key(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_string("lookup-key")
}

fn make_before_update(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_string("before")
}

fn validate_update_operands_after_collection(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let receiver_payload = ctx.arg(0)?;
    let key = ctx.arg(1)?;
    let rhs = ctx.arg(2)?;
    ctx.return_unit();
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(receiver_payload)?, "before");
    assert_eq!(ctx.format_value(key)?, "lookup-key");
    assert_eq!(ctx.format_value(rhs)?, "abcdefgh");
    Ok(())
}

fn return_getter_payload_after_collection(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let payload = ctx.arg(0)?;
    let key = ctx.arg(1)?;
    ctx.return_unit();
    assert!(ctx.collect_garbage()?);
    assert_eq!(ctx.format_value(key)?, "lookup-key");
    assert_eq!(ctx.format_value(payload)?, "abcdefgh");
    ctx.set_return(payload)
}

#[test]
fn custom_apply_update_frames_retain_receiver_key_rhs_and_results_across_collection_and_pause() {
    for pause in ["none", "update", "getter"] {
        let mut pool = TypePool::with_intrinsics();
        let wrapper = pool.register(type_pool::TypeInfo {
            kind: type_pool::TypeKind::Struct {
                name: str_interner::intern("MutableCallable"),
                fields: vec![type_pool::FieldInfo {
                    name: str_interner::intern("text"),
                    ty: Intrinsic::Str.type_index(),
                    offset: 0,
                    has_default: false,
                }],
            },
            type_id: type_pool::TypeId(413, 1),
            size: 8,
            align: 8,
        });
        let update = str_interner::intern("update");
        for (name, function) in [(update, 2), (str_interner::intern("apply"), 3)] {
            pool.add_method(
                wrapper,
                type_pool::MethodSlot {
                    name,
                    func_id: function,
                    trait_impl: None,
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                },
            );
        }
        let mut engine = vm();
        engine.install_type_pool(pool);
        engine.register_builtin(0, make_before_update);
        engine.register_builtin(1, make_lookup_key);
        engine.register_builtin(2, make_string);
        engine.register_builtin(3, collect);
        engine.register_builtin(4, validate_update_operands_after_collection);
        engine.register_builtin(5, return_getter_payload_after_collection);
        engine.register_builtin(6, retain_argument);
        let mut main = if pause == "none" {
            vec![Instruction::call(1, 0)]
        } else {
            vec![
                Instruction::load_imm(Reg(20), 7),
                Instruction::reset(Reg(20), 1, 0),
                Instruction::mov(Reg(21), Reg(0)),
                Instruction::call_builtin(3, 0),
                Instruction::load_unit(Reg(1)),
                Instruction::resume(Reg(21), Reg(1)),
            ]
        };
        main.extend([Instruction::call_builtin(6, 1), Instruction::ret(Reg(0))]);
        add_function(&mut engine, 0, &main, 0);
        add_function(
            &mut engine,
            1,
            &[
                Instruction::new_object(Reg(19), wrapper),
                Instruction::call_builtin(0, 0),
                Instruction::store_field(Reg(19), 0, Reg(0)),
                Instruction::call_builtin(1, 0),
                Instruction::mov(Reg(20), Reg(0)),
                Instruction::call_builtin(2, 0),
                Instruction::mov(Reg(1), Reg(0)),
                Instruction::mov(Reg(0), Reg(20)),
                Instruction::call_method(Reg(19), update.as_u32(), 2),
                Instruction::load_unit(Reg(20)),
                Instruction::load_unit(Reg(1)),
                Instruction::load_unit(Reg(2)),
                // The assigned heap value is now retained only by receiver.text.
                Instruction::call_builtin(3, 0),
                Instruction::call_builtin(1, 0),
                Instruction::call_indirect(Reg(19), 1),
                Instruction::load_unit(Reg(19)),
                Instruction::ret(Reg(0)),
            ],
            0,
        );
        let mut setter = vec![
            Instruction::allocate_slots(3),
            Instruction::store_slot(0, Reg(0)),
            Instruction::store_slot(1, Reg(1)),
            Instruction::store_slot(2, Reg(2)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(1)),
            Instruction::load_unit(Reg(2)),
            Instruction::load_unit(Reg(19)),
            Instruction::load_unit(Reg(20)),
        ];
        if pause == "update" {
            setter.extend([
                Instruction::load_imm(Reg(20), 7),
                Instruction::shift(Reg(20), Reg(0)),
            ]);
        }
        setter.extend([
            Instruction::load_slot(Reg(10), 0),
            Instruction::load_field(Reg(0), Reg(10), 0),
            Instruction::load_slot(Reg(1), 1),
            Instruction::load_slot(Reg(2), 2),
            Instruction::call_builtin(4, 3),
            Instruction::load_slot(Reg(10), 0),
            Instruction::load_slot(Reg(0), 2),
            Instruction::store_field(Reg(10), 0, Reg(0)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(1)),
            Instruction::load_unit(Reg(2)),
            Instruction::load_unit(Reg(10)),
            Instruction::store_slot(0, Reg(0)),
            Instruction::store_slot(1, Reg(0)),
            Instruction::store_slot(2, Reg(0)),
            Instruction::ret(Reg(0)),
        ]);
        add_function(&mut engine, 2, &setter, 3);
        let mut getter = vec![
            Instruction::allocate_slots(1),
            Instruction::load_field(Reg(0), Reg(0), 0),
            Instruction::load_unit(Reg(19)),
            Instruction::call_builtin(5, 2),
            Instruction::store_slot(0, Reg(0)),
            Instruction::load_unit(Reg(0)),
            Instruction::load_unit(Reg(1)),
        ];
        if pause == "getter" {
            getter.extend([
                Instruction::load_imm(Reg(20), 7),
                Instruction::shift(Reg(20), Reg(0)),
            ]);
        }
        getter.extend([Instruction::load_slot(Reg(0), 0), Instruction::ret(Reg(0))]);
        add_function(&mut engine, 3, &getter, 2);
        let task = engine.spawn_root(FuncId(0));
        assert!(matches!(engine.run(), VmResult::Finished), "pause={pause}");
        assert_eq!(engine.task_result_i64(task).unwrap(), 8, "pause={pause}");
        assert_eq!(engine.active_stack_count(), 0);
    }
}
