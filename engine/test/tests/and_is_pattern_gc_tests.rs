//! Computed pattern inputs preserve distinct roots, bindings, and continuation branches.

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
fn computed_enum_bindings_and_aliases_escape_with_both_input_roots() {
    let source = r#"
        enum E{some(text:String),none};global calls:i64=0;
        fn main(){
            let callback=E.some("abcd"++"efgh") match{
                E.some(left) as original and if true{
                    calls+=1;print("");E.some(left++"ij")
                }else{E.none} is E.some(right) as computed if if true{
                    print("");right.len()==10
                }else{false} => ||{
                    print("");
                    if original'type==E and computed'type==E and left.len()==8{
                        right.len()+32
                    }else{0}
                },
                _=>||0
            };
            print("");let answer=callback();print("");
            if calls==1 and answer==42{answer}else{0}
        }
    "#;
    assert_collected_result(source, 5);
}

#[test]
fn computed_tuple_snapshot_keeps_left_and_right_alias_values_distinct() {
    let source = r#"
        fn main(){
            let input=("ab"++"cd",40);
            let callback=input match{
                (text,n) as original and if true{
                    input=("new"++"text",0);print("");(text++"efgh",n+2)
                }else{("",0)} is (more,total) as computed => ||{
                    print("");
                    if original.0.len()==4 and original.1==40 and computed.0.len()==8
                        and more.len()==8 and input.1==0{total}else{0}
                },
                _=>||0
            };
            print("");let answer=callback();print("");answer
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn failed_computed_guard_keeps_its_escaped_rhs_capture_alive() {
    let source = r#"
        enum E{some(text:String),none};
        fn main(){
            let saved:fn()->i64=||0;
            let result=("abcd"++"efgh") match{
                left and E.some(left) is E.some(right) if if true{
                    saved=||{print("");right.len()+34};print("");false
                }else{false}=>0,
                _=>42
            };
            print("");let answer=saved();print("");
            if result==42 and answer==42{answer}else{0}
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn computed_expression_and_internal_guard_multishot_keep_values_and_trace() {
    for (pattern, success, failure, trace, collections) in [
        (
            r#"E.some(left) as original and if true{
                trace=trace*10+1;print("");let n=pause()#;
                E.some(left++if n==0{"ab"}else{"cd"})
            }else{E.none} is E.some(right) as computed if if true{print("");true}else{false}"#,
            r#"let callback=||{print("");
                if original'type==E and computed'type==E and right=="abcdefghab"{
                    trace=trace*10+2;20
                }else{trace=trace*10+3;22}
            };callback()"#,
            "0",
            123,
            8,
        ),
        (
            r#"E.some(left) as original and if true{
                trace=trace*10+1;print("");E.some(left++"ij")
            }else{E.none} is (E.some(right) as computed if if true{
                trace=trace*10+2;keep=||right.len();print("");pause()#==0
            }else{false})"#,
            r#"trace=trace*10+3;let callback=||{print("");
                if original'type==E and computed'type==E{right.len()+10}else{0}
            };callback()"#,
            r#"trace=trace*10+4;print("");keep()+12"#,
            1234,
            7,
        ),
    ] {
        let source = format!(
            r#"
            effect pause(catch k)->i64;global trace:i64=0;
            enum E{{some(text:String),none}};
            fn compute()->i64{{
                let keep:fn()->i64=||0;
                E.some("abcd"++"efgh") match{{
                    {pattern}=>{{{success}}},_=>{{{failure}}}
                }}
            }};
            fn main(){{
                let branch=compute()#{{pause(k)=>k}};print("");
                let first=branch(0).as(i64);print("");
                let second=branch(1).as(i64);print("");
                if first==20 and second==22 and trace=={trace}{{first+second}}else{{0}}
            }}
        "#
        );
        assert_collected_result(&source, collections);
    }
}
