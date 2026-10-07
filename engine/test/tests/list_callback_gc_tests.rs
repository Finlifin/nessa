fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

fn assert_result_with_collections(source: &str, minimum_collections: u64) {
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
    let result = engine.vm_mut().run();
    assert!(
        matches!(result, interpreter::VmResult::Finished),
        "{source}: {result:?}"
    );
    assert!(engine.vm_mut().completed_collections() >= before + minimum_collections);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn callbacks_captures_snapshot_strings_and_results_survive_completed_collections() {
    for (factory, operation) in [
        (
            "fn make(xs:List)->fn(Any)->Any{let prefix=\"ab\"++\"cd\";let f=|x|{xs.pop();print(\"\");prefix++x.as(String)};f}",
            "let result=xs.map(f);result.fold(0,|acc,x|acc+x.as(String).len()).as(i64)+6",
        ),
        (
            "fn make(xs:List)->fn(Any)->bool{let prefix=\"ab\"++\"cd\";let f=|x|{xs.pop();print(\"\");prefix.len()+x.as(String).len()==12};f}",
            "let result=xs.filter(f);result.fold(0,|acc,x|acc+x.as(String).len()).as(i64)+18",
        ),
        (
            "fn make(xs:List)->fn(Any,Any)->Any{let prefix=\"ab\"++\"cd\";let f=|acc,x|{xs.pop();print(\"\");acc.as(String)++prefix++x.as(String)};f}",
            "xs.fold(\"ab\"++\"cd\",f).as(String).len()+2",
        ),
        (
            "fn make(xs:List)->fn(Any)->Unit{let prefix=\"ab\"++\"cd\";let f=|x|{xs.pop();print(\"\");total+=prefix.len()+x.as(String).len()};f}",
            "xs.each(f);total+6",
        ),
        (
            "fn make(xs:List)->fn(Any)->Unit{let prefix=\"ab\"++\"cd\";let f=|x|{xs.pop();print(\"\");total+=prefix.len()+x.as(String).len()};f}",
            "xs.foreach(f);total+6",
        ),
    ] {
        let source = format!(
            "global total:i64=0;{factory};fn main(){{let xs=[\"abcd\"++\"efgh\",\"ijkl\"++\"mnop\",\"qrst\"++\"uvwx\"];let f=make(xs);print(\"\");{operation}}}"
        );
        assert_result_with_collections(&source, 4);
    }
}

#[test]
fn fold_multishot_restores_stack_cursor_and_roots_across_real_collections() {
    let source = r#"
        effect pause(catch k)->i64;
        global entries:i64=0;
        fn compute()->i64{
            let xs=[1,2,3];
            xs.fold(0,|acc,x|{
                entries+=1;
                print("");
                if x==1{pause()#}else{acc+x}
            }).as(i64)
        };
        fn main(){
            let saved=compute()#{pause(k)=>k};
            print("");
            let first=saved(10).as(i64);
            print("");
            let second=saved(20).as(i64);
            if first==15 and second==25 and entries==5{first+second+2}else{0}
        }
    "#;
    assert_result_with_collections(source, 7);
}
