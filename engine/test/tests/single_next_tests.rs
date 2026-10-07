mod common;

#[test]
fn typed_single_next_loops_and_list_nulls_execute() {
    for source in [
        "struct I{n:i64};impl Iterator for I{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){if self.n<3{self.n=self.n+1;IterationStep(i64).yielded(self.n)}else{IterationStep(i64).done}}};fn main(){let sum:i64=0;let iter=I{n:0};for n in iter{sum+=n};sum+36}",
        "fn main(){let count:i64=0;for x in [null,42,(),null]{count+=1};count+38}",
        "fn main(){let sum:i64=0;for x in [1,2,3]{sum+=x.as(i64)};sum+36}",
        "fn main(){let n:i64=0;for null in [null,42,(),null]{n+=1};n+40}",
        "fn main(){let n:i64=0;for x if x==42 in [null,42,(),42]{n+=1};n+40}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn list_iterators_take_independent_shallow_snapshots() {
    for source in [
        "fn main(){let values=[null,(),42];let first=values.into_iter();let second=values.into_iter();values.set(0,99);values.pop();values.push(false);let a=first.next() match{IterationStep(Any).yielded(null)=>20,_=>0};let b=second.next() match{IterationStep(Any).yielded(null)=>22,_=>0};if first.next()'type==IterationStep(Any) and second.next()'type==IterationStep(Any){a+b}else{0}}",
        "fn main(){let values=[20,22];let total:i64=0;for n in values{values.pop();values.push(99);total+=n.as(i64)};total}",
        "fn main(){let values=[null,42];let iter=values.into_iter();let count:i64=0;for _ in iter{count+=1};let a=iter.next();let b=iter.next();let ended=a match{IterationStep(Any).done=>true,_=>false};let still_ended=b match{IterationStep(Any).done=>true,_=>false};if ended and still_ended{count+40}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn wrong_iterator_contracts_and_unused_conversion_obligations_are_rejected() {
    for source in [
        "struct P{};impl Iterator for P{pub fn next(self)->IterationStep(Any){IterationStep(Any).done}};fn main(){42}",
        "struct P{};impl Iterator for P{assoc Item:Type=i64;pub fn next(self)->IterationStep(String){IterationStep(String).done}};fn main(){42}",
        "struct P{};impl Iterator for P{assoc Item:Type=i64;pub fn has_next(self)->bool{false};pub fn next(self)->i64{42}};fn main(){42}",
        "struct P{};impl IntoIterator for P{assoc Iter:Type=i64;pub fn into_iter(self)->i64{42}};fn main(){42}",
        "struct P{};impl IntoIterator for P{pub fn into_iter(self)->P{self}};fn main(){42}",
        "struct P{};fn main(){let p=P{};for x in p{x};42}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(result.into_artifact().is_err(), "emitted {source}");
    }
}

#[test]
fn initialization_tracks_implicit_loop_calls_and_adapter_targets() {
    let source = "trait Total{derive fn total(self)->i64{let iter:Self=self;let sum:i64=0;for n in iter{sum+=n};sum}};struct Cursor{done:bool};impl Iterator for Cursor{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){if self.done{IterationStep(i64).done}else{self.done=true;IterationStep(i64).yielded(storage.value)}}};impl Total for Cursor{};mod api{pub global answer:i64=Cursor{done:false}.total()};mod unused{extend Iterator for Cursor{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){IterationStep(i64).yielded(api.answer)}}};mod storage{pub global value:i64=42};fn main(){api.answer}";
    assert_eq!(common::run_value(source).unwrap(), 42);
    let cycle = source.replace("pub global value:i64=42", "pub global value:i64=api.answer");
    let result = driver::Driver::new().compile(&cycle);
    assert!(result.has_errors);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("cyclic module value initialization dependency")
    }));
    assert!(result.codegen_output.functions.is_empty());
    assert!(result.into_artifact().is_err());
}

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn list_iteration_survives_real_gc_and_multishot_resumption() {
    let source = "effect pause(catch k)->i64;fn compute()->i64{let values=[\"abcd\"++\"efgh\",null];pause()#;let total:i64=0;for item in values{print(\"\");if item==null{total+=13}else{total+=item.as(String).len()}};total};fn main(){let saved=compute()#{pause(k)=>k};print(\"\");let a=saved(0).as(i64);print(\"\");let b=saved(1).as(i64);a+b}";
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
    assert!(engine.vm_mut().completed_collections() >= before + 4);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}
