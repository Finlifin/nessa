mod common;

#[test]
fn concrete_step_factory_constructs_matches_and_reflects_precise_items() {
    for source in [
        "typealias S = IterationStep(i64)\nfn main(){S.yielded(42) match {S.yielded(n)=>n,S.done=>0}}",
        "fn main(){IterationStep(i64).yielded(42) match {IterationStep(i64).yielded(n)=>n,_=>0}}",
        "typealias S = IterationStep(Any)\nfn main(){S.yielded(null) match {S.yielded(null)=>42,S.done=>0,_=>1}}",
        "typealias S = IterationStep(Unit)\nfn main(){S.yielded(()) match {S.yielded(())=>42,S.done=>0}}",
        "typealias S = IterationStep((i64,i64))\nfn main(){S.yielded((40,2)) match {S.yielded((a,b))=>a+b,_=>0}}",
        "typealias Item = i64\ntypealias Factory = IterationStep\ntypealias S = Factory(Item)\nfn main(){if S.yielded(42)'type == IterationStep(i64) and S.done'type == S {42}else{0}}",
        "fn next()->IterationStep(i64){IterationStep(i64).yielded(42)}\nfn main(){next() match {IterationStep(i64).yielded(n)=>n,_=>0}}",
        "mod api{pub typealias Factory=IterationStep;pub typealias S=Factory(i64)}\nuse api.Factory as Step\nfn main(){if Step(i64)==api.S {42}else{0}}",
        "fn IterationStep(x:i64)->i64{x}\nfn main(){IterationStep(42)}",
        "fn main(){IterationStep((i64,i64)).yielded((40,2)) match {IterationStep((i64,i64)).yielded((a,b))=>a+b,_=>0}}",
        "typealias S=IterationStep(IterationStep(i64))\nfn main(){S.yielded(IterationStep(i64).yielded(42)) match {S.yielded(IterationStep(i64).yielded(n))=>n,_=>0}}",
        "typealias S=IterationStep(fn(i64)->i64)\nfn f(x:i64)->i64{x+2}\nfn main(){S.yielded(f) match {S.yielded(f)=>f(40),_=>0}}",
        "use std.builtin.IterationStep as Step\nfn main(){if Step(i64)==IterationStep(i64){42}else{0}}",
        "mod local{pub enum IterationStep{done,yielded(value:i64)}}\nfn main(){if local.IterationStep.done'type!=IterationStep(i64) {local.IterationStep.yielded(42) match {local.IterationStep.yielded(n)=>n,_=>0}}else{0}}",
        "fn main(){typealias S=IterationStep(i64);S.yielded(value=42) match {S.yielded(n)=>n,_=>0}}",
        "typealias S=IterationStep(i64)\nimpl S{pub fn answer(self)->i64{self match{S.yielded(n)=>n,S.done=>0}};pub fn make()->S{S.yielded(42)}}\nfn main(){S.make().answer()}",
        "impl IterationStep(i64){pub fn answer(self)->i64{self match{IterationStep(i64).yielded(n)=>n,_=>0}}}\nfn main(){IterationStep(i64).yielded(42).answer()}",
        "trait Score{fn score(self)->i64}\nimpl Score for IterationStep(i64){pub fn score(self)->i64{self match{IterationStep(i64).yielded(n)=>n,_=>0}}}\nfn main(){IterationStep(i64).yielded(42).score()}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn step_factory_rejects_invalid_applications_and_payloads() {
    for source in [
        "fn main(){IterationStep()}",
        "fn main(){IterationStep(i64,String)}",
        "fn main(){IterationStep(Item=i64)}",
        "fn main(){let item:Type=i64;IterationStep(item)}",
        "fn main(){IterationStep(42)}",
        "fn main(){IterationStep(Display)}",
        "fn main(){let factory=IterationStep;factory(i64)}",
        "fn main()->IterationStep{42}",
        "typealias S=IterationStep(i64)\nfn main(){S.yielded(true)}",
        "typealias S=IterationStep(i64)\nfn main(){S.unknown}",
        "typealias S=IterationStep(i64)\nfn main(){S.yielded}",
        "typealias S=IterationStep(i64)\nfn main(){S.done(42)}",
        "fn main(){.IterationStep'builtin(i64)}",
        "mod std{pub typealias Factory=.IterationStep'builtin}\nfn main(){Factory(i64)}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(
            result.into_artifact().is_err(),
            "emitted invalid source {source}"
        );
    }
}

#[test]
fn bootstrap_factory_identity_is_available_without_standard_library() {
    let source = "typealias Step=IterationStep\ntypealias S=Step(i64)\nfn main(){S.yielded(42)}";
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty());
    let map = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
    let file = map.new_source_file(
        rustc_span::FileName::Custom("factory.ns".into()),
        source.into(),
    );
    let diagnostics = diagnostic::DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    let resolved = resolution::resolve(ast, &diagnostics);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    let plan = resolved.enum_constructions.values().next().unwrap();
    assert_eq!(
        resolved
            .type_pool
            .checked_iteration_step_item(plan.type_index)
            .unwrap(),
        Some(type_pool::Intrinsic::I64.type_index())
    );
}

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn step_payload_survives_real_gc_and_multiple_continuation_resumptions() {
    let source = "typealias S=IterationStep(String);effect pause(catch k)->i64;fn compute()->S{let result:S=S.yielded(\"abcd\"++\"efgh\");pause()#;print(\"\");result};fn main(){let saved=compute()#{pause(k)=>k};print(\"\");let first:S=saved(0).as(S);print(\"\");let second:S=saved(1).as(S);print(\"\");let a=first match{S.yielded(text)=>text.len(),S.done=>0};let b=second match{S.yielded(text)=>text.len(),S.done=>0};a+b+26}";
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
    assert!(engine.vm_mut().completed_collections() >= before + 3);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn dynamic_step_payloads_are_checked_and_different_items_remain_distinct() {
    assert_eq!(common::run_value("typealias S=IterationStep(i64)\nfn main(){let value:Any=42;S.yielded(value) match {S.yielded(n)=>n,_=>0}}").unwrap(),42);
    let error = common::run_value(
        "typealias S=IterationStep(i64)\nfn main(){let value:Any=true;S.yielded(value);0}",
    )
    .unwrap_err();
    assert!(error.contains("TypeError"), "{error}");
    assert_eq!(
        common::run_value("fn main(){if IterationStep(i64)!=IterationStep(String) {42}else{0}}")
            .unwrap(),
        42
    );
}
