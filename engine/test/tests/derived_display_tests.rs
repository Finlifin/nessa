//! Compiler-derived methods are ordinary callable targets in frozen trait tables.
mod common;

#[test]
fn display_wrapper_preserves_legacy_struct_format_through_trait_proof() {
    for source in [
        "struct P{x:i64,label:String};derive Display for P;fn show(x:Display)->String{x.to_string()};fn main(){let p=P{x:42,label:\"hello\"};if p.to_string()==\"P \\x7b x: 42, label: hello \\x7d\" and show(p)==\"P \\x7b x: 42, label: hello \\x7d\"{42}else{0}}",
        "struct Empty{};derive Display for Empty;fn show(x:Display)->String{x.to_string()};fn main(){if show(Empty{})==\"Empty \\x7b  \\x7d\"{42}else{0}}",
        "struct P{x:i64};typealias Item=P;derive Display for Item;fn show(x:Display)->String{x.to_string()};fn main(){if show(Item{x:42})==\"P \\x7b x: 42 \\x7d\"{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn generated_eq_and_partial_eq_targets_execute_through_trait_parameters() {
    for source in [
        "struct P{x:i64,label:String};derive Eq for P;fn same(a:Eq,b:Eq)->bool{a==b and not(a!=b)};fn main(){if same(P{x:42,label:\"a\"},P{x:42,label:\"a\"}) and not same(P{x:42,label:\"a\"},P{x:42,label:\"b\"}){42}else{0}}",
        "enum E{empty,payload(x:i64)};derive Eq for E;fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn main(){if same(E.payload(42),E.payload(42)) and not same(E.empty,E.payload(42)){42}else{0}}",
        "struct P{x:i64};derive PartialEq for P;fn same(a:PartialEq,b:PartialEq)->bool{a==b and not(a!=b)};fn main(){if same(P{x:42},P{x:42}) and not same(P{x:42},P{x:0}){42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

fn collect_at_print(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

fn collect_derived_display(
    context: &mut interpreter::BuiltinCtx<'_>,
) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    let receiver = context.arg(0)?;
    assert!(context.collect_garbage()?);
    context.return_derived_display(receiver)?;
    assert!(context.collect_garbage()?);
    Ok(())
}

#[test]
fn real_collection_preserves_display_receiver_result_and_paused_trait_frames() {
    for source in [
        "struct P{text:String};derive Display for P;fn show(x:Display)->String{x.to_string()};fn main(){let text=show(P{text:\"abcd\"++\"efgh\"});print(\"\");if text==\"P \\x7b text: abcdefgh \\x7d\"{42}else{0}}",
        "effect pause(catch k)->i64;struct P{text:String};derive Display for P;fn work(x:Display)->String{pause()#;x.to_string()};fn main(){let saved=work(P{text:\"abcd\"++\"efgh\"})# {pause(k)=>k};print(\"\");let first=saved(0);let second=saved(1);if first==\"P \\x7b text: abcdefgh \\x7d\" and second==\"P \\x7b text: abcdefgh \\x7d\"{42}else{0}}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(!compiled.has_errors, "{source}: {:?}", compiled.diagnostics);
        let mut engine = initialization::Engine::with_defaults();
        let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
            .unwrap()
            .unwrap();
        engine
            .vm_mut()
            .register_builtin(runtime::ids::PRINT, collect_at_print);
        engine
            .vm_mut()
            .register_builtin(runtime::ids::DERIVED_DISPLAY, collect_derived_display);
        let before = engine.vm_mut().completed_collections();
        let task = engine.vm_mut().spawn_root(entry);
        assert!(
            matches!(engine.vm_mut().run(), interpreter::VmResult::Finished),
            "{source}"
        );
        assert!(engine.vm_mut().completed_collections() >= before + 3);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
