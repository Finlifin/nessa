//! Source entry snapshots and packed containers remain rooted across real GC.

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
    eprintln!(
        "completed collections: {completed}; required: {minimum_collections}; result: 42; active stacks: 0"
    );
}

#[test]
fn heap_entries_and_packed_containers_escape_into_closures_without_losing_sharing() {
    assert_collected_result(
        r#"
        struct Cell{n:i64};
        fn text()->String{let value="abcd"++"efgh";print("");value};
        fn make(...xs:List,...ps:Map)->fn()->i64{
            print("");let callback=ps("callback").as(fn()->i64);
            ||{print("");let cell=xs(0).as(Cell);if ps("cell").as(Cell).n==cell.n{
                callback()+xs(1).as(String).len()
            }else{0}}
        };
        fn main(){let cell=Cell{n:32};let f=make{cell,text(),cell:cell,callback:||cell.n};
            print("");cell.n=34;let answer=f();print("");answer}
    "#,
        5,
    );
}

#[test]
fn multishot_interleaved_entries_keep_independent_container_slots_and_shared_heap() {
    assert_collected_result(
        r#"
        effect pause(catch k)->i64;
        struct Cell{n:i64};
        global entries:i64=0;global after:i64=0;global gathers:i64=0;global trace:i64=0;
        global first_children:List=[];global first_properties:Map=Map();
        fn tail(cell:Cell)->Cell{after+=1;print("");cell};
        fn gather(...xs:List,...ps:Map)->i64{
            gathers+=1;print("");
            let delta=xs(1).as(i64);let cell=ps("cell").as(Cell);
            let answer=xs(0).as(String).len()+cell.n+delta;
            ps("delta")=delta;
            if gathers==1{first_children=xs;first_properties=ps};
            trace=trace*10+delta+1;print("");answer
        };
        fn work(cell:Cell)->i64{
            entries+=1;let text="abcd"++"efgh";print("");print("");
            gather{text,pause()#,cell:tail(cell)}
        };
        fn main(){let cell=Cell{n:12};let saved=work(cell)#{pause(k)=>k};print("");
            let a=saved(0);print("");let b=saved(2);print("");cell.n=14;
            if a==20 and b==22 and entries==1 and after==2 and gathers==2 and trace==13
                and first_children(1)==0 and first_properties("delta")==0
                and first_properties("cell").as(Cell).n==14{a+b}else{0}}
    "#,
        11,
    );
}

#[test]
fn scoped_item_defaults_and_effect_catch_payloads_remain_rooted_through_packing() {
    assert_collected_result(
        r#"
        struct P{text:String};trait Source{
            assoc Item:Type=Self;fn next(self)->Item;
            derive fn gather(self,...xs:List,...ps:Map)->Item{
                let copy:Self=self;let value:Item=copy.next();print("");
                let f=ps("callback").as(fn(Item)->Item);f(value)
            }
        };
        impl Source for P{pub fn next(self)->P{print("");self}};
        effect choose(...xs:List,catch k,...ps:Map)->P;
        fn identity(value:P)->P{print("");value};
        fn main(){let p=P{text:"abcd"++"efgh"};
            let value:P=choose{p,callback:identity}#{choose(xs,k,ps)=>{
                print("");let first=xs(0).as(P);k(first.gather{callback:ps("callback")})
            }};
            print("");value.text.len()+34}
    "#,
        5,
    );
}
