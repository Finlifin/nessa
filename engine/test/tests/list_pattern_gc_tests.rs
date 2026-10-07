//! List snapshots and rest bindings retain shallow roots across guards and continuations.

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
fn prefix_suffix_nested_rest_and_escaped_captures_keep_heap_values_alive() {
    let source = r#"
        enum E{some(text:String)};
        fn length(value:Any)->i64{value.as(E) match{E.some(text)=>text.len()}};
        fn make()->fn()->i64{
            let xs=["abcd"++"efgh",[E.some("ijkl"++"mnop"),4],
                E.some("qrst"++"uvwx"),"yzab"++"cdef"];
            xs match{
                [(prefix if if true{
                    xs.pop();xs.pop();xs.pop();xs.pop();print("");true
                }else{false}),[inside,...inner],...middle,suffix] as original=>||{
                    print("");
                    if original.len()==0 and middle.len()==1 and inner.len()==1{
                        prefix.as(String).len()+length(inside)+length(middle(0))+suffix.as(String).len()+inner(0).as(i64)+6
                    }else{0}
                },
                _=>||0
            }
        };
        fn main(){
            let callback=make();print("");let a=callback();print("");let b=callback();
            if a==42 and b==42{b}else{0}
        }
    "#;
    assert_collected_result(source, 5);
}

#[test]
fn rest_lists_copy_slots_but_share_heap_elements_with_the_original() {
    let source = r#"
        struct Cell{value:i64};
        fn make()->fn()->i64{
            let a=Cell{value:40};let b=Cell{value:2};let xs=[a,b];
            xs match{
                [...tail] as original if if true{
                    let first:Cell=tail(0);first.value=first.value+1;
                    original(0)=Cell{value:0};tail(1)=Cell{value:3};print("");true
                }else{false}=>||{
                    print("");
                    if a.value==41 and b.value==2 and original(0).as(Cell).value==0
                        and original(1).as(Cell).value==2 and tail(0).as(Cell).value==41{
                        tail(0).as(Cell).value+tail(1).as(Cell).value-2
                    }else{0}
                },
                _=>||0
            }
        };
        fn main(){let callback=make();print("");let answer=callback();print("");answer}
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn failed_guard_captures_keep_old_rest_while_later_arms_snapshot_mutated_input() {
    let source = r#"
        fn main(){
            let saved:fn()->i64=||0;
            let xs=["prefix"++"heap","abcd"++"efgh","suffix"++"heap"];
            let selected=xs match{
                [first,...rest,last] if if true{
                    saved=||{print("");rest(0).as(String).len()+34};
                    xs(0)=40;xs(1)=2;xs.pop();print("");false
                }else{false}=>0,
                [40,2]=>42,
                _=>0
            };
            print("");let answer=saved();print("");
            if selected==42 and answer==42{answer}else{0}
        }
    "#;
    assert_collected_result(source, 4);
}

#[test]
fn multishot_for_guards_keep_snapshots_and_share_the_heap_iterator_cursor() {
    let source = r#"
        effect pause(catch k)->i64;
        global visits:i64=0;global trace:i64=0;global bodies:i64=0;global checked:i64=0;
        fn compute()->i64{
            let rows=[["abcd"++"efgh",2],["ijkl"++"mnop",2]];
            let total:i64=0;
            for [head,...rest] as original if if true{
                visits+=1;trace=trace*10+1;
                if visits==1{
                    let row:List=original;row.pop();row.pop();print("");
                    let choice=pause()#;print("");
                    if head.as(String).len()==8 and rest.len()==1 and rest(0)==2{
                        checked+=1;choice==0
                    }else{false}
                }else{print("");true}
            }else{false} in rows{
                bodies+=1;print("");
                if rest'type==List{total+=head.as(String).len()+rest(0).as(i64)}
            };
            total
        };
        fn main(){
            let branch=compute()#{pause(k)=>k};print("");
            let a=branch(0).as(i64);print("");let b=branch(1).as(i64);print("");
            if a==20 and b==0 and visits==2 and bodies==2 and trace==11 and checked==2{a+b+22}else{0}
        }
    "#;
    assert_collected_result(source, 9);
}

#[test]
fn multishot_fold_guards_restore_snapshot_rest_and_frame_local_cursor() {
    let source = r#"
        effect pause(catch k)->i64;
        global visits:i64=0;global trace:i64=0;global bodies:i64=0;
        fn compute()->i64{
            let rows=[["abcd"++"efgh",2],["ijkl"++"mnop",2]];
            let total=rows.fold(0,|acc,row|{
                row match{
                    [head,...rest] as original if if true{
                        visits+=1;trace=trace*10+1;
                        if visits==1{
                            let list:List=original;list.pop();list.pop();print("");pause()#==0
                        }else{print("");true}
                    }else{false}=>{
                        bodies+=1;print("");acc+head.as(String).len()+rest(0).as(i64)
                    },
                    _=>acc
                }
            }).as(i64);
            if total==20{20}else if total==10{22}else{0}
        };
        fn main(){
            let branch=compute()#{pause(k)=>k};print("");
            let a=branch(0).as(i64);print("");let b=branch(1).as(i64);print("");
            if a==20 and b==22 and visits==3 and bodies==3 and trace==111{a+b}else{0}
        }
    "#;
    assert_collected_result(source, 9);
}
