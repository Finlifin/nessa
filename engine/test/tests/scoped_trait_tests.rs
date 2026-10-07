//! Scoped trait evidence follows lexical declarations instead of global registration order.

mod common;

fn executes_42(sources: &[&str]) {
    for source in sources {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

fn rejects(sources: &[(&str, &str)]) {
    for &(source, expected) in sources {
        let driver = driver::Driver::new();
        let result = driver.compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "expected {expected}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn independent_module_and_block_trait_implementations_select_local_methods() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{40}};pub fn answer()->i64{P{}.value()}}\nmod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{P{}.value()}}\nfn main(){a.answer()+b.answer()}",
        "struct P{}\ntrait Read{fn value(self)->i64}\nfn main(){let a=if true{extend Read for P{pub fn value(self)->i64{40}};P{}.value()}else{0};let b=if true{extend Read for P{pub fn value(self)->i64{2}};P{}.value()}else{0};a+b}",
        "mod model{pub struct P{}}\nuse model.P as Item\ntypealias Alias=Item\ntrait Read{fn value(self)->i64}\nextend Read for Alias{pub fn value(self)->i64{42}}\nfn main(){Item{}.value()}",
    ]);
}

#[test]
fn trait_typed_parameters_returns_and_optional_values_use_lexical_evidence() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};fn read(x:Read)->i64{x.value()};pub fn answer()->i64{read(P{})}}\nfn main(){a.answer()}",
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};fn make()->Read{P{}};pub fn answer()->i64{let p:Read=make();p.value()}}\nfn main(){a.answer()}",
        "struct P{}\ntrait Empty{}\nmod a{extend Empty for P{};fn accepts(x:?Empty)->i64{42};pub fn answer()->i64{accepts(P{})}}\nfn main(){a.answer()}",
    ]);
}

#[test]
fn defaults_closures_and_effect_frames_keep_their_declared_trait_evidence() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub fn consume(.x:Read=P{})->i64{x.value()}}\nfn main(){a.consume()}",
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub fn make()->fn()->i64{let p:Read=P{};||p.value()}}\nfn main(){let f=a.make();f()}",
        "effect pause(catch k)->i64\nstruct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub fn work()->i64{let p:Read=P{};pause()#;p.value()}}\nfn main(){let saved=a.work()# {pause(k)=>k};let x=saved(0);let y=saved(1);if x==42 and y==42{42}else{0}}",
    ]);
}

#[test]
fn dynamic_methods_and_comparison_operators_respect_scope() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub fn answer()->i64{let p:Any=P{};p.value()}}\nfn main(){a.answer()}",
        "struct P{}\nmod a{extend Eq for P{pub fn eq(self,other:Self)->bool{true}};pub fn answer()->i64{let p=P{};let q=P{};if p==q{40}else{0}}}\nmod b{extend Eq for P{pub fn eq(self,other:Self)->bool{false}};pub fn answer()->i64{let p=P{};let q=P{};if p!=q{2}else{0}}}\nfn main(){a.answer()+b.answer()}",
    ]);
}

#[test]
fn inherited_traits_accept_forward_visible_parents() {
    executes_42(&[
        "struct P{}\ntrait Base{fn base(self)->i64}\ntrait Mid(Base){fn middle(self)->i64}\ntrait Child(Mid){fn child(self)->i64}\nmod a{extend Child for P{pub fn child(self)->i64{2}};extend Mid for P{pub fn middle(self)->i64{10}};extend Base for P{pub fn base(self)->i64{30}};pub fn answer()->i64{P{}.base()+P{}.middle()+P{}.child()}}\nfn main(){a.answer()}",
        "struct P{}\ntrait Base{fn base(self)->i64}\ntrait Child(Base){fn child(self)->i64}\nmod a{extend Child for P{pub fn child(self)->i64{2}};extend Base for P{pub fn base(self)->i64{40}};pub fn answer()->i64{P{}.base()+P{}.child()}}\nfn main(){a.answer()}",
    ]);
}

#[test]
fn invisible_ambiguous_duplicate_and_parent_evidence_are_rejected() {
    rejects(&[
        (
            "struct P{}\nextend Eq for P{private fn eq(self,other:Self)->bool{true}}\nfn main(){let p=P{};let q=P{};p==q}",
            "not visible",
        ),
        (
            "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}}}\nfn consume(x:Read)->i64{42}\nfn main(){consume(P{})}",
            "does not implement",
        ),
        (
            "struct P{}\ntrait Empty{}\nmod a{extend Empty for P{}}\nfn main(){let p:Empty=P{};42}",
            "does not implement",
        ),
        (
            "struct P{}\ntrait Empty{}\nfn main(){if true{extend Empty for P{};42}else{0};let p:Empty=P{};42}",
            "does not implement",
        ),
        (
            "struct P{}\ntrait Empty{}\nextend Empty for P{}\nextend Empty for P{}\nfn main(){42}",
            "duplicate trait implementation",
        ),
        (
            "struct P{}\nimpl Eq for P{pub fn eq(self,other:Self)->bool{true}}\nextend Eq for P{pub fn eq(self,other:Self)->bool{false}}\nfn main(){let p=P{};let q=P{};p==q}",
            "ambiguous comparison",
        ),
        (
            "struct P{}\ntrait Empty{}\nimpl Empty for P{}\nextend Empty for P{}\nfn main(){let p:Empty=P{};42}",
            "ambiguous",
        ),
        (
            "struct P{}\ntrait Base{}\ntrait Child(Base){}\nmod a{extend Base for P{}}\nmod b{extend Child for P{}}\nfn main(){42}",
            "visible parent trait",
        ),
    ]);
}

#[test]
fn global_derivation_cannot_export_local_field_comparison_evidence() {
    rejects(&[(
        "struct Inner{}\nstruct Outer{x:Inner}\nmod a{extend Eq for Inner{pub fn eq(self,other:Self)->bool{true}};derive Eq for Outer}\nfn main(){42}",
        "field type",
    )]);
    executes_42(&[
        "struct Inner{}\nstruct Outer{x:Inner}\nimpl Eq for Inner{pub fn eq(self,other:Self)->bool{true}}\nmod a{derive Eq for Outer}\nfn main(){let p=Outer{x:Inner{}};let q=Outer{x:Inner{}};if p==q{42}else{0}}",
    ]);
}

#[test]
fn runtime_trait_casts_keep_the_callers_lexical_scope() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub fn answer()->i64{let p:Any=P{};let typed:Read=p;typed.as(Read).value()}}\nfn main(){a.answer()}",
        "struct P{}\ntrait Empty{}\nmod a{extend Empty for P{};pub fn answer()->i64{let p:Any=P{};let typed:Empty=p.as(Empty);42}}\nfn main(){a.answer()}",
    ]);
    let source = "struct P{}\ntrait Empty{}\nmod a{extend Empty for P{}}\nfn main(){let p:Any=P{};let typed=p.as(Empty);42}";
    assert!(matches!(common::run_value(source), Err(error) if error.contains("TypeError")));
}

#[test]
fn module_global_trait_values_use_the_declaration_scope() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub global x:Read=P{};pub fn answer()->i64{x.value()}}\nfn main(){a.answer()}",
    ]);
}

#[test]
fn struct_trait_fields_use_the_current_scope() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nstruct Box{x:Read}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub fn answer()->i64{let b=Box{x:P{}};b.x.value()}}\nfn main(){a.answer()}",
    ]);
}

#[test]
fn enum_trait_payloads_use_the_current_scope() {
    executes_42(&[
        "struct P{}\ntrait Read{fn value(self)->i64}\nenum E{wrapped(x:Read),}\nmod a{extend Read for P{pub fn value(self)->i64{42}};pub fn answer()->i64{let e=E.wrapped(P{});e match{E.wrapped(x)=>x.value()}}}\nfn main(){a.answer()}",
    ]);
}

#[test]
fn escaping_values_do_not_transport_a_local_trait_implementation() {
    executes_42(&[
        "struct P{}\ntrait Empty{}\nmod a{extend Empty for P{};pub global x:Empty=P{};pub fn raw()->Any{x}}\nfn main(){let raw:Any=a.raw();42}",
        "struct P{}\ntrait Empty{}\nmod a{extend Empty for P{};pub global x:Empty=P{}}\nfn main(){let raw:Any=a.x;42}",
    ]);
    for source in [
        "struct P{}\ntrait Empty{}\nstruct Box{x:Empty}\nmod a{extend Empty for P{};pub fn make()->Box{Box{x:P{}}}}\nfn main(){let b=a.make();let raw:Any=b.x;42}",
        "struct P{}\ntrait Empty{}\nenum E{wrapped(x:Empty),}\nmod a{extend Empty for P{};pub fn make()->E{E.wrapped(P{})}}\nfn main(){let e=a.make();e match{E.wrapped(x)=>{let raw:Any=x;42}}}",
        "struct P{}\ntrait Empty{}\nmod a{extend Empty for P{};pub global x:Empty=P{}}\nfn main(){let raw:Any=a.x;let typed=raw.as(Empty);42}",
    ] {
        assert!(
            matches!(common::run_value(source), Err(error) if error.contains("TypeError")),
            "{source}"
        );
    }
}

#[test]
fn scoped_iterator_evidence_is_checked_before_lowering() {
    executes_42(&[
        "struct Cursor{done:bool};struct Source{};mod a{extend IntoIterator for Source{assoc Iter:Type=Cursor;pub fn into_iter(self)->Cursor{Cursor{done:false}}};extend Iterator for Cursor{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){if self.done{IterationStep(i64).done}else{self.done=true;IterationStep(i64).yielded(42)}}};pub fn answer()->i64{let total:i64=0;let source=Source{};for x in source{total+=x};total}};fn main(){a.answer()}",
    ]);
    rejects(&[
        (
            "struct P{};extend Iterator for P{assoc Item:Type=i64;private fn next(self)->IterationStep(i64){IterationStep(i64).done}};fn main(){let p=P{};for x in p{42}}",
            "not visible",
        ),
        (
            "struct P{};impl IntoIterator for P{assoc Iter:Type=P;pub fn into_iter(self)->P{self}};extend IntoIterator for P{assoc Iter:Type=P;pub fn into_iter(self)->P{self}};fn main(){let p=P{};for x in p{42}}",
            "ambiguous iterator",
        ),
        (
            "struct P{};extend IntoIterator for P{assoc Iter:Type=P};fn main(){let p=P{};for x in p{42}}",
            "into_iter",
        ),
        (
            "struct P{};extend Iterator for P{assoc Item:Type=i64};fn main(){let p=P{};for x in p{42}}",
            "next",
        ),
        (
            "struct P{};extend Iterator for P{assoc Item:Type=i64;pub fn next(self)->i64{42}};fn main(){let p=P{};for x in p{42}}",
            "signature",
        ),
        (
            "struct P{};extend IntoIterator for P{assoc Iter:Type=P;pub fn into_iter(self,x:i64)->P{self}};fn main(){let p=P{};for x in p{42}}",
            "signature",
        ),
        (
            "struct P{};impl Iterator for P{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){IterationStep(i64).done}};extend Iterator for P{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){IterationStep(i64).done}};fn main(){let p=P{};for x in p{42}}",
            "ambiguous iterator",
        ),
    ]);
}
