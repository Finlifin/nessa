//! Trait parameters retain the implementation selected by their caller.
mod common;

#[test]
fn scoped_proof_survives_forwarding_to_another_module() {
    let source = "struct P{};trait Read{fn value(self)->i64};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn answer()->i64{b.consume(P{})}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn consume(x:Read)->i64{x.value()};pub fn answer()->i64{consume(P{})}};fn main(){a.answer()+b.answer()}";
    assert_eq!(common::run_value(source).unwrap(), 42);
    let cast = source.replace("x.value()", "x.as(Read).value()");
    assert_eq!(common::run_value(&cast).unwrap(), 42);
}

#[test]
fn separate_trait_names_and_parent_projection_preserve_frozen_slots() {
    for source in [
        "struct P{};trait Left{fn value(self)->i64};trait Right{fn value(self)->i64};impl Left for P{pub fn value(self)->i64{40}};impl Right for P{pub fn value(self)->i64{2}};fn sum(a:Left,b:Right)->i64{a.value()+b.value()};fn main(){sum(P{},P{})}",
        "struct P{};trait Left{fn value(self)->i64};trait Right{fn value(self)->i64};trait Child(Left,Right){};impl Left for P{pub fn value(self)->i64{0}};impl Right for P{pub fn value(self)->i64{0}};impl Child for P{pub fn value(self)->i64{42}};fn right(x:Right)->i64{x.value()};fn child(x:Child)->i64{right(x)};fn main(){child(P{})}",
        "struct P{};trait Base{fn value(self)->i64};trait Child(Base){fn extra(self)->i64};mod a{extend Base for P{pub fn value(self)->i64{2}};extend Child for P{pub fn value(self)->i64{40};pub fn extra(self)->i64{2}};pub fn child(x:Child)->i64{outside.consume(x)+x.extra()};pub fn answer()->i64{child(P{})}};mod outside{pub fn consume(x:Base)->i64{x.value()}};fn main(){a.answer()}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn closures_and_multishot_effect_frames_keep_trait_proofs() {
    for source in [
        "struct P{};trait Read{fn value(self)->i64};mod a{extend Read for P{pub fn value(self)->i64{42}};pub fn capture(x:Read)->fn()->i64{||x.value()};pub fn answer()->i64{let f=capture(P{});f()}};fn main(){a.answer()}",
        "effect pause(catch k)->i64;struct P{};trait Read{fn value(self)->i64};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn work(x:Read)->i64{let n=pause()#;x.value()+n};pub fn answer()->i64{let saved=work(P{})# {pause(k)=>k};let first=saved(2);let second=saved(3);if first==42 and second==43{42}else{0}}};fn main(){a.answer()}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn additional_self_requires_the_same_concrete_implementor() {
    let source = "struct P{};struct Q{};trait Pair{fn same(self,other:Self)->i64};impl Pair for P{pub fn same(self,other:Self)->i64{42}};impl Pair for Q{pub fn same(self,other:Self)->i64{0}};fn use_pair(a:Pair,b:Pair)->i64{a.same(b)};fn main(){use_pair(P{},P{})}";
    assert_eq!(common::run_value(source).unwrap(), 42);
    let wrong = source.replace("use_pair(P{},P{})", "use_pair(P{},Q{})");
    assert!(matches!(common::run_value(&wrong), Err(error) if error.contains("InvalidTraitProof")));
}

fn collect_at_print(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn real_collection_preserves_proof_capture_data_and_detached_frames() {
    for source in [
        "struct P{text:String};trait Read{fn value(self)->i64};mod outside{pub fn capture(x:Read)->fn()->i64{||x.value()}};mod a{extend Read for P{pub fn value(self)->i64{self.text.len()}};pub fn make()->fn()->i64{outside.capture(P{text:\"abcd\"++\"efgh\"})}};fn main(){let f=a.make();print(\"\");f()+34}",
        "effect pause(catch k)->i64;struct P{text:String};trait Read{fn value(self)->i64};mod outside{pub fn work(x:Read)->i64{let n=pause()#;x.value()+n}};mod a{extend Read for P{pub fn value(self)->i64{self.text.len()}};pub fn answer()->i64{let saved=outside.work(P{text:\"abcd\"++\"efgh\"})# {pause(k)=>k};print(\"\");let first=saved(34);let second=saved(35);if first==42 and second==43{42}else{0}}};fn main(){a.answer()}",
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
        assert!(engine.vm_mut().completed_collections() > before);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}

#[test]
fn copies_assignments_and_control_flow_join_the_actual_proof() {
    for body in [
        "let z:Read=if flag{a}else{b};z.value()",
        "var z:Read=a;if flag{z=b};z.value()",
        "let z:Read=a;let copied:Read=z;if flag{copied.value()}else{b.value()}",
    ] {
        let source = format!(
            "struct P{{}};struct Q{{}};trait Read{{fn value(self)->i64}};impl Read for P{{pub fn value(self)->i64{{40}}}};impl Read for Q{{pub fn value(self)->i64{{2}}}};fn choose(a:Read,b:Read,flag:bool)->i64{{{body}}};fn main(){{choose(P{{}},Q{{}},true)+choose(P{{}},Q{{}},false)}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{source}");
    }
}

#[test]
fn explicit_trait_method_arguments_keep_their_own_proofs() {
    let source = "struct P{};trait Right{fn value(self)->i64};trait Left{fn combine(self,other:Right)->i64};impl Right for P{pub fn value(self)->i64{2}};impl Left for P{pub fn combine(self,other:Right)->i64{40+other.value()}};fn combine(a:Left,b:Right)->i64{a.combine(b)};fn main(){combine(P{},P{})}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn explicit_any_erasure_does_not_recover_hidden_trait_authority() {
    for body in [
        "let y:Any=x;consume(y)",
        "var y:Any=x;y=x;consume(y)",
        "let y:Any=x;let f=||consume(y);f()",
    ] {
        let source = format!(
            "struct P{{}};trait Read{{fn value(self)->i64}};mod a{{extend Read for P{{pub fn value(self)->i64{{42}}}};pub fn answer()->i64{{outside.take(P{{}})}}}};mod outside{{fn consume(x:Read)->i64{{x.value()}};pub fn take(x:Read)->i64{{{body}}}}};fn main(){{a.answer()}}"
        );
        assert!(
            matches!(common::run_value(&source), Err(error) if error.contains("TypeError")),
            "{source}"
        );
    }
}

#[test]
fn trait_comparison_uses_the_callers_proof_for_equal_and_not_equal() {
    let source = "struct P{};mod a{extend Eq for P{pub fn eq(self,other:Self)->bool{true}};pub fn answer()->i64{outside.consume(P{},P{})}};mod outside{extend Eq for P{pub fn eq(self,other:Self)->bool{false}};pub fn consume(left:Eq,right:Eq)->i64{if left==right and not(left!=right){42}else{0}}};fn main(){a.answer()}";
    assert_eq!(common::run_value(source).unwrap(), 42);
    let wrong = "struct P{};struct Q{};impl Eq for P{pub fn eq(self,other:Self)->bool{true}};impl Eq for Q{pub fn eq(self,other:Self)->bool{true}};fn consume(left:Eq,right:Eq)->i64{if left==right{42}else{0}};fn main(){consume(P{},Q{})}";
    assert!(matches!(common::run_value(wrong), Err(error) if error.contains("InvalidTraitProof")));
}
