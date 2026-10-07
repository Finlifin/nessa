//! Negated predicates keep private guard captures and outer aliases rooted.

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
fn private_guard_captures_escape_both_successful_and_failed_negated_predicates() {
    for (predicate, expected) in [("false", 1), ("true", 2)] {
        let source = format!(
            r#"
            enum E{{some(text:String),none}};
            fn main(){{
                let saved:fn()->i64=||0;
                let selected=E.some("abcd"++"efgh") match {{
                    not (E.some(text) if if true {{
                        saved=||{{print("");text.len()+34}};
                        print("");{predicate}
                    }}else{{false}})=>1,
                    _=>2
                }};
                print("");let answer=saved();print("");
                if selected=={expected} and answer==42{{answer}}else{{0}}
            }}
        "#
        );
        assert_collected_result(&source, 4);
    }
}

#[test]
fn outer_alias_captures_keep_heap_tuples_alive_after_inner_scope_is_discarded() {
    let source = r#"
        global visits:i64=0;
        fn main(){
            let callback=("abcd"++"efgh",42) match {
                not ((text,n) if if true{visits+=1;n==0}else{false}) as whole => ||{print("");whole.0.len()+34},
                _=>||0
            };
            print("");let first=callback();print("");let second=callback();
            if visits==1 and first==42 and second==42{second}else{0}
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn negated_guard_multishot_restores_private_captures_aliases_and_branch_values() {
    let source = r#"
        effect pause(catch k)->i64;
        global trace:i64=0;
        enum E{some(text:String),none};
        fn compute()->i64{
            let capture:fn()->i64=||0;
            E.some("abcd"++"efgh") match {
                not (E.some(text) if if true{
                    trace=trace*10+1;
                    capture=||text.len();
                    print("");pause()#==0
                }else{false}) as whole => {
                    trace=trace*10+2;
                    let callback=||{
                        print("");whole match{E.some(text)=>text.len()+13,_=>0}
                    };
                    callback()
                },
                _=>{trace=trace*10+3;print("");capture()+13}
            }
        };
        fn main(){
            let branch=compute()#{pause(k)=>k};print("");
            let first=branch(0).as(i64);print("");
            let second=branch(1).as(i64);print("");
            if first==21 and second==21 and trace==132{first+second}else{0}
        }
    "#;
    assert_collected_result(source, 6);
}
