#[test]
fn independent_map_iterators_created_before_capture_keep_snapshot_payloads_alive() {
    fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
        context.require_arity(1)?;
        assert!(context.collect_garbage()?);
        context.return_unit();
        Ok(())
    }
    let source = r#"
        effect pause(catch k)->i64;
        fn compute()->i64 {
            var m:Map=Map();
            m("abcd"++"efgh")="ijkl"++"mnop";
            m("null")=null;
            let left=m.into_iter();
            let right=m.into_iter();
            m=Map();
            let branch=pause()#;
            let iter=if branch==0 {left}else{right};
            let total:i64=0;
            for (key,value) in iter {
                print("");
                if value==null {total+=5}else{total+=key.len()+value.as(String).len()}
            };
            total
        };
        fn main(){let saved=compute()#{pause(k)=>k};print("");let a=saved(0).as(i64);print("");let b=saved(1).as(i64);a+b}
    "#;
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    engine
        .vm_mut()
        .register_builtin(runtime::ids::PRINT, collect);
    let before = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert!(engine.vm_mut().completed_collections() >= before + 4);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}
