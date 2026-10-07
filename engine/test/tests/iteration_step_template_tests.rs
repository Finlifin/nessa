mod common;

#[test]
fn associated_step_signatures_preserve_exact_item_and_nested_shapes() {
    for source in [
        "struct P{};trait Source{assoc Item:Type=Any;fn next(self)->IterationStep(Item)};impl Source for P{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){IterationStep(i64).yielded(42)}};fn main(){P{}.next() match{IterationStep(i64).yielded(n)=>n,_=>0}}",
        "struct P{};trait Source{assoc Item:Type=Any;assoc Result:Type=IterationStep(Item);fn next(self)->Result};impl Source for P{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){IterationStep(i64).yielded(42)}};fn main(){P{}.next() match{IterationStep(i64).yielded(n)=>n,_=>0}}",
        "struct P{x:i64};trait Source{assoc Item:Type=Self;assoc Result:Type=IterationStep(Item);derive fn next(self)->Result{IterationStep(Item).yielded(self)}};impl Source for P{};fn main(){P{x:42}.next() match{IterationStep(P).yielded(value)=>value.x,_=>0}}",
        "struct P{x:i64};trait Source{assoc Result:Type=IterationStep(Self);derive fn next(self)->Result{IterationStep(Self).yielded(self)}};impl Source for P{};fn main(){P{x:42}.next() match{IterationStep(P).yielded(value)=>value.x,_=>0}}",
        "struct P{x:i64};trait Source{derive fn next(self)->IterationStep(Self){IterationStep(Self).yielded(self)}};impl Source for P{};fn main(){P{x:42}.next() match{IterationStep(P).yielded(value)=>value.x,_=>0}}",
        "struct P{};trait Source{assoc Item:Type=i64;derive fn nested(self,value:Item)->(?IterationStep(Item),fn()->IterationStep(Item)){(IterationStep(Item).yielded(value),||->IterationStep(Item){IterationStep(Item).yielded(value)})}};impl Source for P{};fn main(){let result=P{}.nested(21);let first=result.0.as(IterationStep(i64));let second=result.1();let a=first match{IterationStep(i64).yielded(n)=>n,_=>0};let b=second match{IterationStep(i64).yielded(n)=>n,_=>0};a+b}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn default_step_aliases_constructors_patterns_and_type_values_are_specialized() {
    for source in [
        "struct P{x:i64};trait Source{assoc typealias S=IterationStep(Self);derive fn next(self)->S{S.yielded(self)};derive fn result_type(self)->Type{S}};impl Source for P{};fn main(){let p=P{x:0};if p.result_type()==IterationStep(P){P{x:42}.next() match{IterationStep(P).yielded(value)=>value.x,_=>0}}else{0}}",
        "struct P{};trait Source{assoc Item:Type=i64;assoc typealias S=IterationStep(Item);derive fn answer(self,value:Item)->Item{let step:S=S.yielded(value);step match{S.yielded(n)=>n,S.done=>value}};derive fn result_type(self)->Type{IterationStep(Item)}};impl Source for P{};fn main(){let p=P{};if p.result_type()==IterationStep(i64){P{}.answer(42)}else{0}}",
        "struct P{};trait Source{assoc Item:Type=Any;derive fn next(self,value:Item)->IterationStep(Item){IterationStep(Item).yielded(value)}};mod a{extend Source for P{assoc Item:Type=String};pub fn next()->IterationStep(String){P{}.next(\"abcdefgh\")}};mod b{extend Source for P{assoc Item:Type=i64};pub fn answer()->i64{let text=a.next() match{IterationStep(String).yielded(text)=>text.len(),_=>0};let n=P{}.next(34) match{IterationStep(i64).yielded(n)=>n,_=>0};text+n}};fn main(){b.answer()}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn wrong_item_ordinary_enum_and_uninstantiated_runtime_templates_are_rejected() {
    for source in [
        "struct P{};trait Source{assoc Item:Type=i64;fn next(self)->IterationStep(Item)};impl Source for P{pub fn next(self)->IterationStep(String){IterationStep(String).yielded(\"wrong\")}};fn main(){42}",
        "enum Ordinary{done,yielded(value:i64)};struct P{};trait Source{assoc Item:Type=i64;fn next(self)->IterationStep(Item)};impl Source for P{pub fn next(self)->Ordinary{Ordinary.done}};fn main(){42}",
        "trait T{assoc Item:Type=i64};fn main(){IterationStep(T.Item)}",
        "trait T{assoc Item:Type=i64};typealias S=IterationStep(T.Item);fn main(){S}",
        "trait T{derive fn bad(self)->IterationStep(Display){IterationStep(Display).done}};fn main(){42}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(result.into_artifact().is_err(), "emitted {source}");
    }
}

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn dependent_default_payload_survives_gc_and_multiple_resumptions() {
    let source = "effect pause(catch k)->i64;struct P{};trait Source{assoc Item:Type=String;derive fn next(self)->IterationStep(Item){let result=IterationStep(Item).yielded(\"abcd\"++\"efgh\");pause()#;print(\"\");result}};impl Source for P{};typealias S=IterationStep(String);fn main(){let saved=P{}.next()#{pause(k)=>k};print(\"\");let first:S=saved(0).as(S);print(\"\");let second:S=saved(1).as(S);print(\"\");let a=first match{S.yielded(text)=>text.len(),S.done=>0};let b=second match{S.yielded(text)=>text.len(),S.done=>0};a+b+26}";
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
