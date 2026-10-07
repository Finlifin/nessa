//! Effect operation results constrain resumes independently of handler answers.

mod common;

use diagnostic::Level;

fn rejected(source: &str) {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "accepted {source}");
    assert!(compiled.codegen_output.functions.is_empty(), "{source}");
    assert!(
        compiled.diagnostics.iter().any(|diagnostic| {
            diagnostic.level == Level::Error && diagnostic.message.contains("type mismatch")
        }),
        "{source}: {:?}",
        compiled.diagnostics
    );
}

#[test]
fn stable_catch_bindings_aliases_clones_and_captured_closures_reject_wrong_inputs() {
    for body in [
        "k(true)",
        "let alias=k;alias(true)",
        "let a=k;let b=a;b(true)",
        "let copied=k.clone();copied(true)",
        "let invoke=||{k(true)};invoke()",
        "let alias=k;let invoke=||{alias.clone()(true)};invoke()",
    ] {
        rejected(&format!(
            "effect choose(catch k)->i64;fn main(){{choose()#{{choose(k)=>{{{body}}}}}}}"
        ));
    }
}

#[test]
fn every_ordinary_handler_exit_is_checked_at_its_own_effect_boundary() {
    for body in [
        "return true;42",
        "return true if true;42",
        "resume true;42",
        "resume true if true;42",
        "if true{return true}else{42}",
    ] {
        rejected(&format!(
            "effect ask()->i64;fn main(){{ask()#{{ask()=>{{{body}}}}}}}"
        ));
    }
}

#[test]
fn nested_function_and_handler_returns_keep_separate_contracts() {
    for source in [
        "effect ask()->i64;fn main(){ask()#{ask()=>{let f:fn()->bool=||{return true};if f(){42}else{0}}}}",
        "effect ask()->i64;effect flag()->bool;fn result(){ask()#{ask()=>{let b=flag()#{flag()=>{return true}};if b{42}else{0}}}};fn main(){result()}",
        "effect ask()->i64;fn result(){let n=ask()#{ask()=>{return 40}};n+2};fn main(){result()}",
        "effect text(catch k)->String;fn compute(){text()#.len()+34};fn main(){compute()#{text(k)=>{let alias=k;alias.clone()(\"ab\"++\"cdefgh\")}}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn string_optional_null_unit_and_narrow_literal_resume_inputs_are_valid() {
    for source in [
        "effect small(catch k)->i8;fn main(){let n=small()#{small(k)=>k(42)};if type_of(n)==i8{n.as(i64)}else{0}}",
        "effect small()->i8;fn main(){let n=small()#{small()=>{resume 42}};if type_of(n)==i8{n.as(i64)}else{0}}",
        "effect maybe(catch k)->?i64;fn main(){let n=maybe()#{maybe(k)=>k(42)};if n==42{42}else{0}}",
        "effect maybe(catch k)->?String;fn main(){let n=maybe()#{maybe(k)=>k(null)};if n==null{42}else{0}}",
        "effect tick(catch k)->Unit;fn main(){tick()#{tick(k)=>k(())};42}",
        "effect tick()->Unit;fn main(){tick()#{tick()=>{return ()}};42}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn erased_mutable_returned_and_cloned_continuations_check_even_ignored_results() {
    for source in [
        "effect choose(catch k)->i64;fn invoke(k:Continuation){k(true);42};fn main(){let saved=choose()#{choose(k)=>k};invoke(saved)}",
        "effect choose(catch k)->i64;fn invoke(k:Any){k(true);42};fn main(){let saved=choose()#{choose(k)=>k};invoke(saved)}",
        "effect choose(catch k)->i64;fn identity(k:Continuation)->Continuation{k};fn main(){let saved=choose()#{choose(k)=>k};identity(saved).clone()(true);42}",
        "effect choose(catch k)->i64;fn main(){choose()#{choose(k)=>{var alias=k;alias=k;alias(true);42}}}",
        "effect choose(catch k)->i64;struct Box{value:Continuation};fn main(){let saved=choose()#{choose(k)=>k};let box=Box{value:saved};box.value.clone()(true);42}",
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}

#[test]
fn dynamic_handler_exits_arguments_and_guards_reject_incompatible_values() {
    for source in [
        "effect ask()->i64;fn wrong()->Any{true};fn main(){ask()#{ask()=>{return wrong();42}};42}",
        "effect ask()->i64;fn wrong()->Any{true};fn main(){ask()#{ask()=>{resume wrong();42}};42}",
        "effect ask()->i64;fn wrong()->Any{true};fn main(){ask()#{ask()=>{resume wrong() if true;42}};42}",
        "effect ask()->i64;fn wrong()->Any{true};fn main(){ask()#{ask()=>wrong()};42}",
        "effect ask(x:i64)->i64;fn wrong()->Any{true};fn main(){ask(wrong())#{ask(x)=>42};42}",
        "effect ask()->i64;fn wrong()->Any{42};fn main(){ask()#{ask()=>{resume 42 if wrong();42}}}",
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}

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
    assert!(engine.vm_mut().completed_collections() >= before + minimum);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn typed_heap_input_delayed_clones_and_multishot_answers_survive_completed_gc() {
    collected(
        r#"
        effect text(catch k)->String;
        fn compute(){let prefix="ab"++"cd";let value=text()#;print("");prefix.len()+value.len()}
        fn main(){
            let saved=compute()#{text(k)=>k};print("");
            let copy=saved.clone();print("");
            let first=saved("ab"++"cdefgh").as(i64);print("");
            let second=copy("ijkl"++"mnop").as(i64);print("");
            if first==12 and second==12{first+second+18}else{0}
        }
    "#,
        6,
    );
}

#[test]
fn recapture_preserves_heterogeneous_continuation_answers_and_heap_roots() {
    collected(
        r#"
        effect choose(catch k)->i64;
        fn compute(){let text="ab"++"cdefgh";let a=choose()#;print("");a+choose()#+text.len()}
        fn main(){
            let first=compute()#{choose(k)=>k};print("");
            let second=first(10);print("");
            let answer=second(24).as(i64);print("");answer
        }
    "#,
        4,
    );
}

#[test]
fn integer_resume_contract_keeps_string_branch_answers_and_integer_handler_answers() {
    collected(
        r#"
        effect choose(catch k)->i64;
        fn compute()->String{
            let n=choose()#;print("");
            if n==1{"ab"++"cdefgh"}else{"ijkl"++"mnop"}
        }
        fn main(){
            compute()#{choose(k)=>{
                let alias=k;print("");
                let first=alias(1).as(String);print("");
                let second=alias.clone()(2).as(String);print("");
                if first=="abcdefgh" and second=="ijklmnop"{first.len()+second.len()+26}else{0}
            }}
        }
    "#,
        5,
    );
}
