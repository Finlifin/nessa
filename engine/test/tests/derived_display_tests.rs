//! Compiler-derived methods are ordinary callable targets in frozen trait tables.
mod common;

#[test]
fn compositional_struct_display_formats_through_trait_proof_and_nested_fields() {
    for source in [
        "struct P{x:i64,label:String};derive Display for P;fn show(x:Display)->String{x.to_string()};fn main(){let p=P{x:42,label:\"hello\"};if p.to_string()==\"P \\x7b x: 42, label: \\\"hello\\\" \\x7d\" and show(p)==\"P \\x7b x: 42, label: \\\"hello\\\" \\x7d\"{42}else{0}}",
        "struct Empty{};derive Display for Empty;fn show(x:Display)->String{x.to_string()};fn main(){if show(Empty{})==\"Empty \\x7b  \\x7d\"{42}else{0}}",
        "struct P{x:i64};typealias Item=P;derive Display for Item;fn show(x:Display)->String{x.to_string()};fn main(){if show(Item{x:42})==\"P \\x7b x: 42 \\x7d\"{42}else{0}}",
        "struct Tag{id:i64};impl Display for Tag{pub fn to_string(self)->String{\"#\"++self.id.to_string()}};struct Row{tag:Tag,note:?String,pair:(String,i64)};derive Display for Row;fn main(){let a=Row{tag:Tag{id:7},note:\"ok\",pair:(\"hi\",3)};let b=Row{tag:Tag{id:8},note:null,pair:(\"yo\",4)};if a.to_string()==\"Row \\x7b tag: #7, note: \\\"ok\\\", pair: (\\\"hi\\\", 3) \\x7d\" and b.to_string()==\"Row \\x7b tag: #8, note: null, pair: (\\\"yo\\\", 4) \\x7d\"{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn compositional_struct_display_rejects_fields_without_checked_global_display() {
    for source in [
        "struct Raw{value:Any};derive Display for Raw;fn main(){0}",
        "struct Bag{items:List};derive Display for Bag;fn main(){0}",
        "struct Dict{entries:Map};derive Display for Dict;fn main(){0}",
        "struct Plain{x:i64};struct Holder{inner:Plain};derive Display for Holder;fn main(){0}",
        "struct Scoped{x:i64};mod local{extend Display for Scoped{pub fn to_string(self)->String{\"scoped\"}};struct Wrapper{inner:Scoped};derive Display for Wrapper};fn main(){0}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "unexpectedly accepted: {source}");
        assert!(
            result.diagnostics.iter().any(|diag| {
                diag.message
                    .contains("Display derivation requires a checked global Display implementation")
            }),
            "{source}: {:?}",
            result.diagnostics
        );
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

#[test]
fn real_collection_preserves_display_receiver_result_and_paused_trait_frames() {
    for (source, min_collections) in [
        (
            "struct Leaf{text:String};impl Display for Leaf{pub fn to_string(self)->String{print(\"\");self.text}};struct P{first:Leaf,second:Leaf};derive Display for P;fn show(x:Display)->String{x.to_string()};fn main(){let text=show(P{first:Leaf{text:\"abcd\"++\"efgh\"},second:Leaf{text:\"ijkl\"++\"mnop\"}});print(\"\");if text==\"P \\x7b first: abcdefgh, second: ijklmnop \\x7d\"{42}else{0}}",
            3,
        ),
        (
            "effect pause(catch k)->i64;struct Leaf{text:String};impl Display for Leaf{pub fn to_string(self)->String{print(\"\");self.text}};struct P{leaf:Leaf};derive Display for P;fn work(x:Display)->String{pause()#;x.to_string()};fn main(){let saved=work(P{leaf:Leaf{text:\"abcd\"++\"efgh\"}})# {pause(k)=>k};print(\"\");let first=saved(0);let second=saved(1);if first==\"P \\x7b leaf: abcdefgh \\x7d\" and second==\"P \\x7b leaf: abcdefgh \\x7d\"{42}else{0}}",
            3,
        ),
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
        let before = engine.vm_mut().completed_collections();
        let task = engine.vm_mut().spawn_root(entry);
        assert!(
            matches!(engine.vm_mut().run(), interpreter::VmResult::Finished),
            "{source}"
        );
        assert!(engine.vm_mut().completed_collections() >= before + min_collections);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
