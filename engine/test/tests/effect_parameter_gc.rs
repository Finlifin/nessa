//! Bound effect arguments and defaults remain live through completed collections.

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

fn collected(source: &str, minimum: u64) {
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
    let completed = engine.vm_mut().completed_collections() - before;
    assert!(
        completed >= minimum,
        "{source}: only {completed} completed collections"
    );
    assert_eq!(
        engine.vm_mut().task_result_i64(task).unwrap(),
        42,
        "{source}"
    );
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn heap_defaults_packed_lists_null_and_unit_survive_default_and_handler_gc() {
    collected(
        r#"
        global calls:i64=0;
        fn make()->String{calls+=1;let text="ab"++"cdefgh";print("");text};
        effect packet(...xs:List,.text:String=make(),.copy:List=[text,xs(0)],.get:fn()->String=||text)->i64;
        fn main(){
            let answer=packet("ijkl"++"mnop",null,())#{packet(xs,text,copy,get)=>{
                print("");
                let valid=xs.len()==3 and xs(1)==null and xs(2)==() and copy(0)==text and copy(1)==xs(0) and get()==text;
                print("");
                if valid and calls==1{text.len()+xs(0).as(String).len()+copy.len()+24}else{0}
            }};
            print("");answer
        }
    "#,
        4,
    );
}

#[test]
fn captured_default_completes_before_the_outer_effect_dispatch() {
    collected(
        r#"
        global calls:i64=0;
        effect seed(catch k)->String;
        effect read(.text:String=make())->i64;
        fn make()->String{
            calls+=1;
            let value=seed()#{seed(k)=>{print("");k("ab"++"cdefgh")}};
            print("");value.as(String)
        };
        fn main(){
            let answer=read()#{read(text)=>{print("");if calls==1{text.len()+34}else{0}}};
            print("");answer
        }
    "#,
        4,
    );
}

#[test]
fn cloned_multishot_branches_preserve_parameters_and_never_repeat_defaults() {
    collected(
        r#"
        global calls:i64=0;
        global correct:bool=false;
        fn make()->String{calls+=1;let text="ab"++"cdefgh";print("");text};
        effect choose(...xs:List,catch k,.text:String=make(),.copy:List=[text,xs(0)])->i64;
        fn compute(){let prefix="ab"++"cd";let value=choose("ijkl"++"mnop")#;print("");value+prefix.len()};
        fn main(){
            let saved=compute()#{choose(xs,k,text,copy)=>{
                print("");
                correct=text=="abcdefgh" and copy(0)==text and copy(1)==xs(0) and xs.len()==1;
                k
            }};
            print("");let cloned=saved.clone();print("");
            let first=saved(8).as(i64);print("");
            let second=cloned(18).as(i64);print("");
            if correct and calls==1 and first==12 and second==22{first+second+8}else{0}
        }
    "#,
        8,
    );
}

#[test]
fn source_order_snapshots_survive_gc_and_supplied_names_suppress_defaults() {
    collected(
        r#"
        global trace:i64=0;
        fn step(n:i64,text:String)->String{trace=trace*10+n;let value=text++"efgh";print("");value};
        effect read(a:String,.b:String=step(3,"qrst"),.c:String=step(4,"unused"))->i64;
        fn main(){
            let answer=read(c=step(2,"ijkl"),a=step(1,"abcd"))#{read(a,b,c)=>{
                print("");
                if trace==213 and a=="abcdefgh" and b=="qrstefgh" and c=="ijklefgh"{a.len()+b.len()+c.len()+18}else{0}
            }};
            print("");answer
        }
    "#,
        5,
    );
}
