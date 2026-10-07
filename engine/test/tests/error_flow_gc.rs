//! Error envelopes root exactly their payload slot through real completed collection.
use std::cell::RefCell;

use type_pool::{TypeId, TypeKind};

#[derive(Default)]
struct Observations {
    envelopes: u64,
    moved: u64,
    pointer_like_tags: u64,
    scalar_i64_tags: u64,
}
thread_local! {static OBSERVED:RefCell<Observations>=RefCell::new(Observations::default());}

fn envelope(
    context: &interpreter::BuiltinCtx<'_>,
    value: runtime::TaggedValue,
) -> Result<Option<(usize, TypeId)>, interpreter::VmError> {
    let Some(pointer) = value.as_heap_ptr() else {
        return Ok(None);
    };
    // SAFETY: an invocation root retains this live managed value during the builtin
    // operation. No collection or allocation occurs while inspecting its header.
    let header = unsafe { gc::ObjectHeader::from_payload_ptr(pointer) };
    if (header.gc_meta >> 19) & 7 != 1 {
        return Ok(None);
    }
    assert_eq!(header.payload_words(), 3, "authenticated ErrorEnvelope ABI");
    assert!(matches!(
        context.type_pool().get(header.type_index).kind,
        TypeKind::ErrorQualified { .. }
    ));
    // SAFETY: the role and checked 3-word length above authorize these two raw
    // identity words. They are numbers, never addresses or ordinary GC slots.
    let (hi, lo) = unsafe {
        let words = pointer.cast::<u64>();
        (words.read(), words.add(1).read())
    };
    let tag = TypeId(hi, lo);
    if tag != TypeId::ZERO {
        let concrete = context
            .type_pool()
            .lookup_by_id(tag)
            .expect("actual stable payload TypeId");
        assert_eq!(context.type_pool().get(concrete).type_id, tag);
    }
    Ok(Some((pointer as usize, tag)))
}

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    let root = context.arg_rooted(0)?;
    let before = envelope(context, context.load_rooted(&root)?)?;
    assert!(context.collect_garbage()?);
    let after = envelope(context, context.load_rooted(&root)?)?;
    match (before, after) {
        (Some((old, tag)), Some((new, actual))) => {
            assert_eq!(
                tag, actual,
                "both full128 tag halves survive moving collection"
            );
            OBSERVED.with(|observed| {
                let mut observed = observed.borrow_mut();
                observed.envelopes += 1;
                observed.moved += u64::from(old != new);
                observed.pointer_like_tags +=
                    u64::from(tag != TypeId::ZERO && (tag.hi() & 7 == 0 || tag.lo() & 7 == 0));
                observed.scalar_i64_tags += u64::from(tag == TypeId(0x4e45_5353_4149_4e00, 9));
            });
        }
        (None, None) => {}
        _ => panic!("envelope role lost during collection"),
    }
    context.return_unit();
    Ok(())
}

fn collected(source: &str, minimum: u64, require_movement: bool, require_scalar_tag: bool) {
    OBSERVED.with(|s| *s.borrow_mut() = Observations::default());
    let completed = execute_collections(source, minimum, collect);
    OBSERVED.with(|s| {
        let s = s.borrow();
        assert!(s.envelopes > 0, "must inspect real managed wrappers");
        if require_movement {
            assert!(s.moved > 0, "no observed physical envelope move");
        }
        if require_scalar_tag {
            assert!(s.scalar_i64_tags > 0 && s.pointer_like_tags > 0,
                "actual full128 i64 tag must resemble an aligned pointer without being scanned");
        }
        eprintln!("completed={completed}, envelope observations={}, physical moves={}, pointer-like tags={}, scalar i64 tags={}",
            s.envelopes, s.moved, s.pointer_like_tags, s.scalar_i64_tags);
    });
}

fn execute_collections(source: &str, minimum: u64, builtin: interpreter::BuiltinFn) -> u64 {
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{source}: {:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    engine
        .vm_mut()
        .register_builtin(runtime::ids::PRINT, builtin);
    let before = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    let result = engine.vm_mut().run();
    assert!(
        matches!(result, interpreter::VmResult::Finished),
        "{source}: {result:?}"
    );
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
    let completed = engine.vm_mut().completed_collections() - before;
    assert!(
        completed >= minimum,
        "completed {completed}, required {minimum}"
    );
    completed
}

#[test]
fn heap_error_payload_aliases_and_escaped_callbacks_remain_shallow_shared() {
    collected(
        r#"struct Cell{n:i64,text:String};enum E{value(cell:Cell)};
        fn make(cell:Cell)->fn()->i64{let value:!E i64=error E.value(cell);print(value);
            value match{error E.value(c)=>||{print(value);print(value);c.n+c.text.len()},_! =>||0}}
        fn main(){let cell=Cell{n:32,text:"abcd"++"efgh"};let callback=make(cell);print("");cell.n=34;callback()}"#,
        4,
        false,
        false,
    );
}

#[test]
fn raw_full128_scalar_tag_words_are_not_gc_references() {
    collected(
        r#"fn main(){let value:!i64 i64=error 42;print(value);print(value);print(value);print(value);
        value match{error n=>n.as(i64),_! =>0}}"#,
        4,
        false,
        true,
    );
}

#[test]
fn failed_guard_capture_keeps_original_envelope_and_heap_payload_alive() {
    collected(
        r#"enum E{value(text:String)};fn main(){var saved:fn()->i64=||0;
        let value:!E i64=error E.value("abcd"++"efgh");
        let selected=value!{E.value(text) if if true{saved=||{print(value);text.len()+34};print(value);false}else{true}=>0,E.* =>42,_! =>0};
        print(value);let answer=saved();print(value);if selected==42{answer}else{0}}"#,
        4,
        false,
        false,
    );
}

#[test]
fn multishot_propagation_preserves_error_exit_and_independent_success_branches() {
    collected(
        r#"enum E{bad};effect pick(catch k)->!E i64;global entries:i64=0;global trace:i64=0;
        fn work()->!E i64{entries+=1;let text="abcd"++"efgh";print("");print("");let n=pick()#!;
            trace=trace*10+n;print("");print("");n+text.len()+12}
        fn main(){let saved=work()#{pick(k)=>k};print("");let failed:!E i64=saved(error E.bad).as(!E i64);print(failed);
            let a:!E i64=saved(0).as(!E i64);print(a);let b:!E i64=saved(2).as(!E i64);print(b);
            let error_seen=failed match{error E.bad=>true,_! =>false};let first=a!{E.* =>0};let second=b!{E.* =>0};
            if error_seen and first==20 and second==22 and entries==1 and trace==2{first+second}else{0}}"#,
        10,
        false,
        false,
    );
}

#[test]
fn resumed_error_guard_reuses_rooted_snapshot_and_preserves_branch_trace() {
    collected(
        r#"enum E{value(text:String)};effect pause(catch k)->bool;global entries:i64=0;global trace:i64=0;
        fn work()->i64{let value:!E i64=error E.value("abcd"++"efgh");
            value!{E.value(text) if if true{entries+=1;print(value);pause()#}else{false}=>{trace=trace*10+1;print(value);text.len()+12},
                E.* =>{trace=trace*10+2;print(value);22},_! =>0}}
        fn main(){let saved=work()#{pause(k)=>k};print("");let a=saved(true).as(i64);print("");let b=saved(false).as(i64);print("");
            if a==20 and b==22 and entries==1 and trace==12{a+b}else{0}}"#,
        6,
        false,
        false,
    );
}

#[test]
fn fragmented_heap_observes_real_envelope_movement_with_string_payload_intact() {
    if isolated_moving_test(
        "fragmented_heap_observes_real_envelope_movement_with_string_payload_intact",
        "NESSA_ERROR_MOVEMENT_CHILD",
    ) {
        return;
    }
    collected(
        r#"fn fragment(){var n:i64=0;while n<128{let garbage="abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz"++"ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ";n+=1};()}
        fn main(){fragment();let value:!String i64=error ("abcd"++"efgh");fragment();print(value);fragment();print(value);fragment();print(value);fragment();print(value);
            value match{error text=>text.as(String).len()+34,_! =>0}}"#,
        4,
        true,
        false,
    );
}

fn check_corrupted_tag_and_payload(
    context: &mut interpreter::BuiltinCtx<'_>,
) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    let value = context.arg(0)?;
    let pointer = value.as_heap_ptr().expect("real ErrorEnvelope");
    assert_eq!(
        envelope(context, value)?.unwrap().1,
        TypeId(0x4e45_5353_4149_4e00, 9)
    );
    assert_eq!(context.format_value(value)?, "error 42");
    for word in 0..3 {
        // SAFETY: this argument is a rooted, checked three-word envelope. The VM
        // operation is exclusive and no GC, VM allocation, or suspension occurs
        // while the single field is invalid. Restore before any further action.
        let old = unsafe { pointer.cast::<u64>().add(word).read() };
        let bad = if word == 2 {
            runtime::TaggedValue::TRUE.raw()
        } else {
            old ^ 1
        };
        unsafe {
            pointer.cast_mut().cast::<u64>().add(word).write(bad);
        }
        let inspected = context.format_value(value);
        unsafe {
            pointer.cast_mut().cast::<u64>().add(word).write(old);
        }
        let error = inspected.expect_err("forged full tag or mismatched payload accepted");
        assert!(format!("{error:?}").contains("TypeError"), "{error:?}");
    }
    assert_eq!(context.format_value(value)?, "error 42");
    collect(context)
}

#[test]
fn checked_inspector_rejects_each_forged_tag_half_and_wrong_payload_before_collection() {
    let source = "fn main(){let value:!i64 i64=error 42;print(value);print(value);print(value);print(value);value match{error n=>n.as(i64),_! =>0}}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    engine
        .vm_mut()
        .register_builtin(runtime::ids::PRINT, check_corrupted_tag_and_payload);
    let before = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
    assert!(engine.vm_mut().completed_collections() - before >= 4);
}

#[test]
fn nested_delimiters_preserve_envelope_roots_when_inner_continuation_branches_resume_outer() {
    collected(
        r#"enum E{bad};effect choose(catch k)->!E i64;effect inner(catch k)->i64;global entries:i64=0;global handlers:i64=0;global trace:i64=0;
        fn work()->!E i64{entries+=1;let persistent:!E i64=error E.bad;print(persistent);
            let value:!E i64=choose()#{choose(k)=>{handlers+=1;print(persistent);let bump=inner()#;print(persistent);k(bump)}};
            let n=value!;trace=trace*10+n;print(persistent);n+20}
        fn main(){let saved=work()#{inner(k)=>k};print("");let a:!E i64=saved(0).as(!E i64);print(a);let b:!E i64=saved(2).as(!E i64);print(b);
            let first=a!{E.* =>0};let second=b!{E.* =>0};if entries==1 and handlers==1 and trace==2 and first==20 and second==22{first+second}else{0}}"#,
        9,
        false,
        false,
    );
}

fn isolated_moving_test(name: &str, child: &str) -> bool {
    // MMTk configuration is process-global; set it before initialization in a
    // fresh process, without changing the parent process's environment.
    if std::env::var_os(child).is_some() {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--test-threads=1", "--nocapture"])
        .env(child, "1")
        .env("MMTK_IMMIX_ALWAYS_DEFRAG", "true")
        .env("MMTK_IMMIX_DEFRAG_EVERY_BLOCK", "true")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("isolated moving collection: {stdout}\n{stderr}");
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "isolated movement fixture failed: {}",
        output.status
    );
    true
}

#[derive(Default)]
struct NativeObservations {
    calls: u64,
    expired: u64,
    moves: u64,
}
thread_local! {
    static NATIVE: RefCell<NativeObservations> = RefCell::new(NativeObservations::default());
    static SAVED_ROOT: RefCell<Option<interpreter::BuiltinRoot>> = const { RefCell::new(None) };
}

fn legacy_retained_argument(
    context: &mut interpreter::BuiltinCtx<'_>,
) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    let copied = context.arg(0)?;
    let root = context.arg_rooted(0)?;
    let before = envelope(context, copied)?.unwrap();
    assert_eq!(context.format_value(copied)?, "error abcdefgh");
    context.return_string("replacement in argument register zero")?;
    for _ in 0..2 {
        assert!(context.collect_garbage()?);
        assert_eq!(
            envelope(context, context.load_rooted(&root)?)?.unwrap(),
            before,
            "legacy handed-out heap address must stay precisely pinned after register overwrite"
        );
        assert_eq!(context.format_value(copied)?, "error abcdefgh");
    }
    context.set_return(copied)?;
    assert_eq!(context.arg(0)?.raw(), copied.raw());
    NATIVE.with(|s| s.borrow_mut().calls += 1);
    context.return_unit();
    Ok(())
}

#[test]
fn legacy_native_argument_survives_register_overwrite_and_completed_moving_collections() {
    if isolated_moving_test(
        "legacy_native_argument_survives_register_overwrite_and_completed_moving_collections",
        "NESSA_ERROR_LEGACY_NATIVE_CHILD",
    ) {
        return;
    }
    NATIVE.with(|s| *s.borrow_mut() = NativeObservations::default());
    execute_collections(
        "fn main(){let value:!String i64=error (\"abcd\"++\"efgh\");print(value);print(value);print(value);print(value);value match{error text=>text.as(String).len()+34,_! =>0}}",
        8,
        legacy_retained_argument,
    );
    NATIVE.with(|s| assert_eq!(s.borrow().calls, 4));
}

fn movable_retained_argument(
    context: &mut interpreter::BuiltinCtx<'_>,
) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    if let Some(previous) = SAVED_ROOT.with(|saved| saved.borrow_mut().take()) {
        assert!(
            matches!(
                context.load_rooted(&previous),
                Err(interpreter::VmError::TypeError)
            ),
            "completed invocation handle must be rejected even when its former slot index is reused"
        );
        NATIVE.with(|s| s.borrow_mut().expired += 1);
    }
    let root = context.arg_rooted(0)?;
    let clone = root.clone();
    let before = envelope(context, context.load_rooted(&root)?)?.unwrap();
    assert_eq!(
        context.format_value(context.load_rooted(&root)?)?,
        "error abcdefgh"
    );
    context.return_string("replacement in argument register zero")?;
    for _ in 0..2 {
        assert!(context.collect_garbage()?);
        let current = context.load_rooted(&root)?;
        assert_eq!(current.raw(), context.load_rooted(&clone)?.raw());
        assert_eq!(envelope(context, current)?.unwrap().1, before.1);
        assert_eq!(context.format_value(current)?, "error abcdefgh");
    }
    let current = context.load_rooted(&root)?;
    let after = envelope(context, current)?.unwrap();
    context.set_return(current)?;
    SAVED_ROOT.with(|saved| *saved.borrow_mut() = Some(clone));
    NATIVE.with(|s| {
        let mut s = s.borrow_mut();
        s.calls += 1;
        s.moves += u64::from(before.0 != after.0);
    });
    context.return_unit();
    Ok(())
}

#[test]
fn movable_native_handles_reload_after_overwrite_and_reject_expired_cross_vm_handles() {
    if isolated_moving_test(
        "movable_native_handles_reload_after_overwrite_and_reject_expired_cross_vm_handles",
        "NESSA_ERROR_MOVABLE_NATIVE_CHILD",
    ) {
        return;
    }
    NATIVE.with(|s| *s.borrow_mut() = NativeObservations::default());
    SAVED_ROOT.with(|s| *s.borrow_mut() = None);
    let source = "fn main(){let value:!String i64=error (\"abcd\"++\"efgh\");print(value);print(value);print(value);print(value);value match{error text=>text.as(String).len()+34,_! =>0}}";
    execute_collections(source, 8, movable_retained_argument);
    // The saved token from the first finished/dropped VM cannot authorize a
    // later VM's native invocation, even if the same root slot index is reused.
    execute_collections(source, 8, movable_retained_argument);
    NATIVE.with(|s| {
        let s = s.borrow();
        assert_eq!(s.calls, 8);
        assert_eq!(s.expired, 7);
        assert!(
            s.moves > 0,
            "movable native handle never followed an actual envelope move"
        );
        eprintln!(
            "native calls={}, expired rejected={}, physical moves={}",
            s.calls, s.expired, s.moves
        );
    });
    SAVED_ROOT.with(|s| *s.borrow_mut() = None);
}
