//! Default bodies execute through concrete adapters and frozen caller evidence.
mod common;

#[test]
fn default_sum_uses_the_selected_implementation() {
    for source in [
        "struct P{};trait Read{fn value(self)->i64;derive fn sum(self)->i64{self.value()+2}};impl Read for P{pub fn value(self)->i64{40}};fn consume(x:Read)->i64{x.sum()};fn main(){consume(P{})}",
        "struct P{};trait Read{fn value(self)->i64;derive fn sum(self)->i64{self.value()+2}};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn answer()->i64{outside.consume(P{})}};mod outside{extend Read for P{pub fn value(self)->i64{2}};pub fn consume(x:Read)->i64{x.sum()}};fn main(){a.answer()}",
        "struct P{};trait Read{fn value(self)->i64;derive fn sum(self)->i64{self.value()+2}};impl Read for P{pub fn value(self)->i64{0};pub fn sum(self)->i64{42}};fn consume(x:Read)->i64{x.sum()};fn main(){consume(P{})}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn inherited_default_keeps_child_overrides_when_projected_to_parent() {
    let source = "struct P{};trait Base{fn value(self)->i64;derive fn sum(self)->i64{self.value()+2}};trait Child(Base){};impl Base for P{pub fn value(self)->i64{2}};impl Child for P{pub fn value(self)->i64{40}};fn base(x:Base)->i64{x.sum()};fn child(x:Child)->i64{base(x)};fn main(){child(P{})}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn default_body_can_call_another_default_and_capture_receiver() {
    for source in [
        "struct P{};trait Read{fn value(self)->i64;derive fn increment(self)->i64{self.value()+1};derive fn sum(self)->i64{self.increment()+1}};impl Read for P{pub fn value(self)->i64{40}};fn consume(x:Read)->i64{x.sum()};fn main(){consume(P{})}",
        "struct P{};trait Read{fn value(self)->i64;derive fn sum(self)->i64{let f=||self.value();f()+2}};impl Read for P{pub fn value(self)->i64{40}};fn consume(x:Read)->i64{x.sum()};fn main(){consume(P{})}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn additional_self_keeps_its_own_frozen_implementation() {
    let source = "struct P{};trait Read{fn value(self)->i64;derive fn sum(self,other:Self)->i64{self.value()+other.value()}};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn answer()->i64{outside.start(P{})}};mod outside{extend Read for P{pub fn value(self)->i64{2}};pub fn start(x:Read)->i64{x.sum(P{})}};fn main(){a.answer()}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn concrete_default_self_results_and_type_values_are_specialized() {
    for source in [
        "struct P{x:i64};trait Read{fn value(self)->i64;derive fn identity(self)->Self{self}};impl Read for P{pub fn value(self)->i64{self.x}};fn accept(p:P)->i64{p.x};fn main(){let p:P=P{x:42}.identity();accept(p)}",
        "struct P{};struct Q{};typealias View=Read;trait Read{fn value(self)->i64;derive fn own_type(self)->Type{Self};derive fn explicit_type(self)->Type{View}};impl Read for P{pub fn value(self)->i64{40}};impl Read for Q{pub fn value(self)->i64{2}};fn main(){let p=P{};let q=Q{};if p.own_type()==type_of(p) and q.own_type()==type_of(q) and p.explicit_type()==Read and q.explicit_type()==Read{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn nested_lambda_self_types_and_copied_receiver_preserve_specialization() {
    for source in [
        "struct P{x:i64};trait Read{fn value(self)->i64;derive fn identity(self)->Self{let f=||->Self{self};f()}};impl Read for P{pub fn value(self)->i64{self.x}};fn accept(p:P)->i64{p.x};fn main(){let p:P=P{x:42}.identity();accept(p)}",
        "struct P{};struct Q{};trait Read{fn value(self)->i64;derive fn own_type(self)->Type{let f=||{let inner=||Self;inner()};f()}};impl Read for P{pub fn value(self)->i64{40}};impl Read for Q{pub fn value(self)->i64{2}};fn main(){let p=P{};let q=Q{};if p.own_type()==type_of(p) and q.own_type()==type_of(q){42}else{0}}",
        "struct P{};trait Base{fn value(self)->i64;derive fn sum(self)->i64{let y:Self=self;y.value()+2}};trait Child(Base){};impl Base for P{pub fn value(self)->i64{2}};impl Child for P{pub fn value(self)->i64{40}};fn base(x:Base)->i64{x.sum()};fn child(x:Child)->i64{base(x)};fn main(){child(P{})}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn typed_self_callback_acquires_other_proof_at_its_actual_caller() {
    let source = "struct P{};trait Read{fn value(self)->i64;derive fn callback(self)->fn(Self)->i64{|other:Self|self.value()+other.value()}};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn make()->fn(P)->i64{P{}.callback()}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{let f=a.make();f(P{})}};fn main(){b.answer()}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn inferred_lambda_and_named_function_types_preserve_self_provenance() {
    let source = "struct P{};struct Q{};trait Read{derive fn callback(self)->fn()->Self{||self};derive fn named(self)->fn(Self)->Self{fn inner(other:Self)->Self{other};inner};derive fn own_type(self)->Type{fn inner()->Type{Self};inner()}};impl Read for P{};impl Read for Q{};typealias PF=fn()->P;typealias QF=fn()->Q;typealias RF=fn()->Read;typealias PN=fn(P)->P;typealias QN=fn(Q)->Q;fn main(){let p=P{};let q=Q{};let pf=p.callback();let qf=q.callback();let pn=p.named();let qn=q.named();if type_of(pf)==PF and type_of(qf)==QF and type_of(pf)!=RF and type_of(pn)==PN and type_of(qn)==QN and type_of(pn(P{}))==P and type_of(qn(Q{}))==Q and p.own_type()==P and q.own_type()==Q{42}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn inferred_self_copies_tuples_and_consistent_branches_are_specialized() {
    for body in [
        "let x=self;||x",
        "||if true{self}else{self}",
        "let pair=(self,self);||pair.0",
    ] {
        let source = format!(
            "struct P{{}};struct Q{{}};trait Read{{derive fn callback(self)->fn()->Self{{{body}}}}};impl Read for P{{}};impl Read for Q{{}};typealias PF=fn()->P;typealias QF=fn()->Q;fn main(){{let p=P{{}}.callback();let q=Q{{}}.callback();if type_of(p)==PF and type_of(q)==QF and type_of(p())==P and type_of(q())==Q{{42}}else{{0}}}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{source}");
    }
}

#[test]
fn nested_recursive_named_functions_specialize_without_capturing_outer_locals() {
    let source = "struct P{};trait Read{derive fn recurse(self)->fn(Self,i64)->Self{fn inner(other:Self,n:i64)->Self{if n==0{other}else{inner(other,n-1)}};inner}};impl Read for P{};typealias PF=fn(P,i64)->P;fn main(){let f=P{}.recurse();if type_of(f)==PF and type_of(f(P{},3))==P{42}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
    let invalid = "struct P{};trait Read{derive fn capture(self)->fn()->Self{fn inner()->Self{self};inner}};impl Read for P{};fn main(){P{}.capture()}";
    let compiled = driver::Driver::new().compile(invalid);
    assert!(compiled.has_errors);
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("cannot capture")),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn explicit_trait_lambda_result_is_not_rewritten_as_implementor_self() {
    let source = "struct P{};trait Read{derive fn callback(self)->fn()->Read{||->Read{self}}};impl Read for P{};typealias RF=fn()->Read;typealias PF=fn()->P;fn main(){let f=P{}.callback();if type_of(f)==RF and type_of(f)!=PF{42}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn explicit_casts_mixed_branches_and_variable_writes_do_not_invent_self_results() {
    for body in [
        "||self.as(Read)",
        "||if true{self}else{other}",
        "var x=self;x=other;||x",
    ] {
        let source = format!(
            "struct P{{}};trait Read{{derive fn callback(self,other:Read)->fn()->Read{{{body}}}}};impl Read for P{{}};typealias RF=fn()->Read;typealias PF=fn()->P;fn main(){{let f=P{{}}.callback(P{{}});if type_of(f)==RF and type_of(f)!=PF{{42}}else{{0}}}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{source}");
    }
}

fn collect_at_print(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn real_collection_preserves_default_body_captures_and_multishot_frames() {
    for source in [
        "struct P{text:String};trait Read{fn value(self)->i64;derive fn sum(self)->i64{let f=||self.value();print(\"\");f()+34}};mod a{extend Read for P{pub fn value(self)->i64{self.text.len()}};pub fn answer()->i64{outside.consume(P{text:\"abcd\"++\"efgh\"})}};mod outside{pub fn consume(x:Read)->i64{x.sum()}};fn main(){a.answer()}",
        "effect pause(catch k)->i64;struct P{text:String};trait Read{fn value(self)->i64;derive fn sum(self)->i64{let n=pause()#;self.value()+n}};mod a{extend Read for P{pub fn value(self)->i64{self.text.len()}};pub fn answer()->i64{let saved=outside.consume(P{text:\"abcd\"++\"efgh\"})# {pause(k)=>k};print(\"\");let first=saved(34);let second=saved(35);if first==42 and second==43{42}else{0}}};mod outside{pub fn consume(x:Read)->i64{x.sum()}};fn main(){a.answer()}",
        "struct P{text:String};trait Read{fn value(self)->i64;derive fn callback(self)->fn()->i64{let x=self;||{let y=x;y.value()}}};mod a{extend Read for P{pub fn value(self)->i64{self.text.len()+32}};pub fn make()->fn()->i64{P{text:\"abcd\"++\"efgh\"}.callback()}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{let f=a.make();print(\"\");f()+2}};fn main(){b.answer()}",
        "effect pause(catch k)->i64;struct P{text:String};trait Read{fn value(self)->i64;derive fn callback(self)->fn()->i64{let x=self;||{let n=pause()#;x.value()+n}}};mod a{extend Read for P{pub fn value(self)->i64{self.text.len()+32}};pub fn make()->fn()->i64{P{text:\"abcd\"++\"efgh\"}.callback()}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{let f=a.make();let saved=f()# {pause(k)=>k};print(\"\");let first=saved(2);let second=saved(3);if first==42 and second==43{42}else{0}}};fn main(){b.answer()}",
        "struct P{text:String};trait Read{derive fn callback(self)->fn()->Self{let x=self;||x}};impl Read for P{};typealias PF=fn()->P;fn main(){let f=P{text:\"abcd\"++\"efgh\"}.callback();print(\"\");let p:P=f();if type_of(f)==PF and type_of(p)==P and p.text.len()==8{42}else{0}}",
        "effect pause(catch k)->i64;struct P{text:String};trait Read{derive fn callback(self)->fn()->Self{let x=self;||{pause()#;x}}};impl Read for P{};typealias PF=fn()->P;fn main(){let f=P{text:\"abcd\"++\"efgh\"}.callback();let saved=f()# {pause(k)=>k};print(\"\");let first:P=saved(0);let second:P=saved(1);if type_of(f)==PF and type_of(first)==P and type_of(second)==P and first.text.len()==8 and second.text.len()==8{42}else{0}}",
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
fn an_explicit_interface_closure_cannot_claim_a_concrete_self_return_contract() {
    let source = "struct P{};struct Q{};trait Read{derive fn callback(self,other:Read)->fn()->Self{||other}};impl Read for P{};impl Read for Q{};fn main(){let f=P{}.callback(Q{});f();42}";
    let result = common::run_value(source);
    assert!(
        matches!(&result, Err(error) if error.contains("TypeError")),
        "{source}: {result:?}"
    );
}

#[test]
fn contextual_lambda_parameters_follow_source_self_and_interface_annotations() {
    let source = "struct P{x:i64};struct Q{x:i64};trait Read{derive fn callback(self)->fn(Self)->Self{|other|other};derive fn explicit(self)->fn(Read)->Read{|other|other}};impl Read for P{};impl Read for Q{};fn main(){let pf=P{x:0}.callback();let qf=Q{x:0}.callback();let erased=P{x:0}.explicit();if type_of(pf)==(fn(P)->P) and type_of(qf)==(fn(Q)->Q) and type_of(erased)==(fn(Read)->Read){pf(P{x:40}).x+qf(Q{x:2}).x}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn inferred_default_lambda_return_tracks_a_source_self_method_result() {
    let source = "struct P{x:i64};trait Read{derive fn identity(self)->Self{self};derive fn callback(self)->fn()->Self{||self.identity()}};impl Read for P{};fn main(){let f=P{x:42}.callback();if type_of(f)==(fn()->P){f().x}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}
