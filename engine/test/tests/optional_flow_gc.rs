//! Optional payload roots survive completed collection and multishot control flow.

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

fn assert_collected_result(source: &str, minimum_collections: u64) {
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{source}: {:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    engine
        .vm_mut()
        .register_builtin(runtime::ids::PRINT, collect);
    let before = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    let result = engine.vm_mut().run();
    assert!(
        matches!(result, interpreter::VmResult::Finished),
        "{source}: {result:?}"
    );
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    let completed = engine.vm_mut().completed_collections() - before;
    assert!(
        completed >= minimum_collections,
        "expected at least {minimum_collections} collections, completed {completed}"
    );
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn propagation_and_unwrap_keep_exact_shared_heap_and_closure_payloads_rooted() {
    let source = r#"
        struct Cell{n:i64,text:String};
        fn payload(cell:Cell)->?Cell{print("");cell};
        fn callback(cell:Cell)->?fn()->i64{
            let value=payload(cell)?;print("");
            ||{print("");value.n+value.text.len()}
        };
        fn main(){
            let cell=Cell{n:32,text:"abcd"++"efgh"};
            let value:?fn()->i64=callback(cell);print("");
            cell.n=34;let f=value.unwrap();f()
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn optional_some_guard_captures_escape_failed_predicates_and_survive_collection() {
    let source = r#"
        fn main(){
            var saved:fn()->i64=||0;
            let value:?String="abcd"++"efgh";
            let selected=value match{
                text? if if true{saved=||{print("");text.len()+34};print("");false}else{true}=>0,
                next? as whole if next==whole=>42,
                _=>0
            };
            print("");let answer=saved();print("");
            if selected==42{answer}else{0}
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn multishot_propagation_preserves_independent_null_exit_and_payload_branch_control() {
    let source = r#"
        effect pause(catch k)->?i64;
        global trace:i64=0;
        global entries:i64=0;
        fn work()->?i64{
            entries+=1;let text="abcd"++"efgh";print("");print("");
            let value=pause()#?;
            print("");trace=trace*10+value;print("");
            value+text.len()+12
        };
        fn main(){
            let saved=work()#{pause(k)=>k};print("");
            let missing=saved(null);print("");
            let first=saved(0);print("");
            let second=saved(2);print("");
            if missing==null and first==20 and second==22 and trace==2 and entries==1{first+second}else{0}
        }
    "#;
    assert_collected_result(source, 10);
}
