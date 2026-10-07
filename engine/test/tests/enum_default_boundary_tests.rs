mod common;

#[test]
fn selected_enum_defaults_drive_initialization_and_explicit_overrides_do_not() {
    for source in [
        "enum E{value(.n:i64=storage.n)};mod api{pub global answer:E=E.value()};mod storage{pub global n:i64=42};fn main(){api.answer match{E.value(n)=>n}}",
        "enum E{value(.n:i64=api.answer)};mod api{pub global answer:i64=E.value(n=42) match{E.value(n)=>n}};fn main(){api.answer}",
        "enum E{value(n:i64,.m:i64=n+2)};global answer:E=E.value(40);fn main(){answer match{E.value(n,m)=>m}}",
        "enum E{value(n:i64,.m:i64=n+2)};struct P{};trait Read{assoc Item:Type=i64;fn item(self)->Item;derive fn packet(self)->E{E.value(self.item())}};impl Read for P{pub fn item(self)->i64{40}};fn main(){P{}.packet() match{E.value(n,m)=>m}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    let cyclic = "enum E{value(.n:i64=storage.n)};mod api{pub global answer:E=E.value()};mod storage{pub global n:i64=api.answer match{E.value(n)=>n}};fn main(){api.answer match{E.value(n)=>n}}";
    let result = driver::Driver::new().compile(cyclic);
    assert!(result.has_errors);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("cyclic module value initialization dependency")
    }));
    assert!(result.codegen_output.functions.is_empty());
    assert!(result.into_artifact().is_err());
}

#[test]
fn enum_defaults_share_expansion_and_control_boundary_rejections() {
    for (source, message) in [
        (
            "enum E{value(.n:E=E.value())};fn main(){42}",
            "recursive default argument expansion",
        ),
        (
            "enum E{value(.n:i64=make())};fn make(.value:E=E.value())->i64{42};fn main(){42}",
            "recursive default argument expansion",
        ),
        (
            "enum E{value(.n:Box=Box{})};struct Box{value:E=E.value()};fn main(){42}",
            "recursive default argument expansion",
        ),
        (
            "enum E{value(.n:i64=if true{return 42}else{0})};fn main(){42}",
            "default boundary",
        ),
        (
            "enum E{value(.n:i64=if true{break;42}else{0})};fn main(){42}",
            "require a loop inside",
        ),
        (
            "enum E{value(.n:i64=if true{continue;42}else{0})};fn main(){42}",
            "require a loop inside",
        ),
        (
            "enum E{value(.n:i64=true)};fn main(){42}",
            "parameter default",
        ),
        (
            "enum E{value(.n:i64=true match{true=>if true{return 42}else{0},_=>0})};fn main(){E.value() match{E.value(n)=>n}}",
            "default boundary",
        ),
        (
            "fn value(.n:i64=true match{true=>if true{return 42}else{0},_=>0})->i64{n};fn main(){value()}",
            "default boundary",
        ),
        (
            "struct Value{n:i64=true match{true=>if true{return 42}else{0},_=>0}};fn main(){Value{}}",
            "default boundary",
        ),
        (
            "enum E{value(.n:i64=true match{true=>if true{break;42}else{0},_=>0})};fn main(){42}",
            "require a loop inside",
        ),
        (
            "enum E{value(.n:i64=true match{true=>if true{continue;42}else{0},_=>0})};fn main(){42}",
            "require a loop inside",
        ),
        (
            "enum E{value(x:i64,...ys:List,z:i64)};fn main(){42}",
            "must follow all fixed",
        ),
        (
            "enum E{value(...a:List,...b:List)};fn main(){42}",
            "dual variadic",
        ),
        (
            "enum E{value(self)};fn main(){E.value(42)}",
            "enum fields require",
        ),
        (
            "fn outer(n:i64){enum E{value(.m:i64=n)};mod child{global answer:E=E.value()}};fn main(){42}",
            "shared scope initialization cannot capture function-local values",
        ),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "{source}: {:?}",
            result.diagnostics
        );
        assert!(result.codegen_output.functions.is_empty());
        assert!(result.into_artifact().is_err());
    }
}

#[test]
fn ordinary_match_arms_share_loop_targets_and_handlers_keep_their_own_context() {
    for source in [
        "enum E{value(.n:i64=if true{let n:i64=0;while true{true match{true=>if true{n+=1;if n<3{continue};break}else{0},_=>0}};n+39}else{0})};fn main(){E.value() match{E.value(n)=>n}}",
        "effect supply()->i64;enum E{value(.n:i64=supply()#{supply()=>if true{return 42}else{0}})};fn main(){E.value() match{E.value(n)=>n}}",
        "enum E{value(.n:i64=(||true match{true=>if true{return 42}else{0},_=>0})())};fn main(){E.value() match{E.value(n)=>n}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn default_any_payloads_remain_checked_at_runtime() {
    for source in [
        "fn wrong()->Any{true};enum E{value(.n:i64=wrong())};fn main(){E.value()}",
        "fn wrong()->Any{false};enum E{value(n:i64,.m:i64=n)};fn main(){E.value(wrong())}",
    ] {
        assert!(common::run_value(source).unwrap_err().contains("TypeError"));
    }
}

#[test]
fn default_closures_and_variadic_payloads_survive_gc_and_multishot() {
    fn collect(ctx: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
        ctx.require_arity(1)?;
        assert!(ctx.collect_garbage()?);
        ctx.return_unit();
        Ok(())
    }
    let source = r#"
    effect pause(catch k)->i64;
    enum E{packet(text:String,...items:List,.callback:fn()->String=||text)};
    fn compute()->i64 {
        let packet=E.packet("abcd"++"efgh","ijkl"++"mnop",null,());
        pause()#;
        print("");
        packet match {
            E.packet(text,items,callback)=> {
                print("");
                if callback()==text and items.len()==3 and items(1)==null and items(2)==() {
                    callback().len()+items(0).as(String).len()+5
                }else{0}
            }
        }
    };
    fn main(){let saved=compute()#{pause(k)=>k};print("");let a=saved(0).as(i64);print("");let b=saved(1).as(i64);a+b}
    "#;
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), result.into_artifact().unwrap())
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
