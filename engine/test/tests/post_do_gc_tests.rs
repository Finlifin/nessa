//! Trailing callbacks preserve ordinary closure roots and continuation semantics.

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
    assert!(engine.vm_mut().completed_collections() >= before + minimum_collections);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn trailing_callbacks_keep_heap_captures_snapshots_and_results_rooted() {
    let source = r#"
        fn main(){
            let prefix="ab"++"cd";
            let xs=["abcd"++"efgh","ijkl"++"mnop","qrst"++"uvwx"];
            print("");
            let result=xs.map do |x|{
                xs.pop();
                print("");
                prefix++x.as(String)
            };
            print("");
            let length=(result.fold(0) do |acc,x|{acc+x.as(String).len()}).as(i64);
            if xs.len()==0 and result.len()==3{length+6}else{0}
        }
    "#;
    assert_collected_result(source, 5);
}

#[test]
fn trailing_block_returns_a_closure_whose_heap_capture_outlives_its_factory() {
    let source = r#"
        fn keep(f:fn()->i64)->fn()->i64{f};
        fn make()->fn()->i64{
            let text="ab"++"cd";
            keep do {print("");text.len()+38}
        };
        fn main(){
            let callback=make();
            print("");
            let first=callback();
            print("");
            let second=callback();
            if first==42 and second==42{second}else{0}
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn trailing_callback_snapshots_copy_slots_and_share_heap_elements() {
    let source = r#"
        struct Cell{value:i64};
        fn main(){
            let first=Cell{value:40};let second=Cell{value:2};
            let xs=[first,second];
            print("");
            let result=xs.map do |x|{
                xs.pop();
                let cell=x.as(Cell);cell.value=cell.value+1;
                print("");cell
            };
            print("");
            if xs.len()==0 and first.value==41 and second.value==3 and
                result(0).as(Cell).value==41 and result(1).as(Cell).value==3{
                first.value+second.value-2
            }else{0}
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn trailing_fold_multishot_restores_cursor_and_roots_between_branches() {
    let source = r#"
        effect pause(catch k)->i64;
        global entries:i64=0;
        fn compute()->i64{
            ([1,2,3].fold(0) do |acc,x|{
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
    assert_collected_result(source, 7);
}
