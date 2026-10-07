mod common;

#[test]
fn alternatives_and_aliases_share_successful_bindings() {
    for source in [
        "fn main(){ 2 match {1 or 2 or 3=>42,_=>0} }",
        "enum E{a(value:i64),b(value:i64)}\nfn main(){ E.a(42) match {E.a(n) or E.b(n)=>n} }",
        "enum E{a(value:i64),b(value:i64)}\nfn main(){ E.b(42) match {E.a(n) or E.b(n)=>n} }",
        "enum E{a(value:i64),b(value:i64)}\nfn main(){ E.b(42) match {(E.a(n) or E.b(n)) as whole=>if whole'type==E {n}else{0}} }",
        "fn main(){ (40,2) match {(a,b) as pair=>a+b+pair.0-40} }",
        "enum E{a(value:i64),b(value:i64)}\nfn main(){ let x:Any=E.b(42); x match {(E.a(n) or E.b(n)) as whole=>if whole'type==E {n}else{0},_=>0} }",
        "enum E{a(value:i64),b(value:i64)}\nfn main(){let f=E.a(42) match {E.a(n) or E.b(n)=>||n};f()} ",
        "enum E{a(value:i64),b(value:i64)}\nfn main(){let f=E.b(42) match {E.a(n) or E.b(n)=>||n};f()} ",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn invalid_composite_binding_contracts_do_not_produce_artifacts() {
    for (source, expected) in [
        (
            "enum E{a(value:i64),b(value:i64)}\nfn main(){ E.a(42) match {E.a(x) or E.b(y)=>42} }",
            "same names",
        ),
        (
            "enum E{a(value:i64),b(value:String)}\nfn main(){ E.a(42) match {E.a(x) or E.b(x)=>42} }",
            "same type",
        ),
        (
            "fn main(){(1,2) match {(x,x)=>42}}",
            "same name more than once",
        ),
        (
            "fn main(){42 match {x as x=>42}}",
            "same name more than once",
        ),
        ("fn main(){42 match {42 as _=>42}}", "identifier binding"),
        (
            "fn main(){42 match {42 as (a,b)=>42}}",
            "identifier binding",
        ),
        ("fn main(){let x or y=42;x}", "supported in match"),
        ("fn main(){let x as y=42;x}", "supported in match"),
        (
            "enum E{a(pair:(i64,i64))}\nfn main(){E.a((1,2)) match {E.a((x if y>0,y))=>42}}",
            "undefined",
        ),
        (
            "enum E{a(pair:(i64,i64)),b(pair:(i64,i64))}\nfn main(){E.a((1,2)) match {E.a((x,y)) or E.b((x if y>0,y))=>42}}",
            "undefined",
        ),
        ("fn main(){42 matches (x as whole); whole}", "undefined"),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled.codegen_output.functions.is_empty(),
            "produced code for {source}"
        );
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|d| d.message.contains(expected)),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
}

#[test]
fn associated_default_adapters_keep_exact_alternative_and_alias_types() {
    let source = r#"
        struct P{};struct Q{};
        trait Source{
            assoc Item:Type=Any;
            fn next(self)->IterationStep(Item);
            fn fallback(self)->Item;
            derive fn callback(self)->fn()->Item{
                self.next() match {
                    (IterationStep(Item).yielded(value) as whole if whole'type==IterationStep(Item) and false)
                    or IterationStep(Item).yielded(value) as whole => ||value,
                    _=>||self.fallback()
                }
            }
        };
        impl Source for P{
            assoc Item:Type=i64;
            pub fn next(self)->IterationStep(i64){IterationStep(i64).yielded(42)};
            pub fn fallback(self)->i64{0}
        };
        impl Source for Q{
            assoc Item:Type=String;
            pub fn next(self)->IterationStep(String){IterationStep(String).yielded("abcd"++"efgh")};
            pub fn fallback(self)->String{""}
        };
        fn main(){
            let p:fn()->i64=P{}.callback();let q:fn()->String=Q{}.callback();
            if p'type==fn()->i64 and q'type==fn()->String and q()=="abcdefgh"{p()}else{0}
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn trait_receiver_pattern_alias_preserves_frozen_evidence() {
    let source = r#"
        struct P{};
        trait Read{
            fn value(self)->i64;
            derive fn callback(self)->fn()->i64{
                self match { _ as saved=>||saved.value() }
            }
        };
        mod a{
            extend Read for P{pub fn value(self)->i64{40}};
            pub fn callback()->fn()->i64{P{}.callback()}
        };
        mod b{
            extend Read for P{pub fn value(self)->i64{2}};
            pub fn answer()->i64{let f=a.callback();f()+P{}.value()}
        };
        fn main(){b.answer()}
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn guard_continuation_branches_and_gc_keep_alias_and_alternative_payloads_alive() {
    fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
        context.require_arity(1)?;
        assert!(context.collect_garbage()?);
        context.return_unit();
        Ok(())
    }
    let source = r#"
        effect pause(catch k)->i64;
        enum E{a(text:String),b(text:String)};
        fn compute()->i64{
            let callback=E.b("abcd"++"efgh") match {
                (E.a(text) or E.b(text)) as whole if if true {print("");pause()#==0}else{false} => ||{
                    print("");if whole'type==E{text.len()+13}else{0}
                },
                E.b(text)=>||{print("");text.len()+13},
                _=>||0
            };
            callback()
        };
        fn main(){
            let saved=compute()#{pause(k)=>k};print("");
            let a=saved(0).as(i64);print("");let b=saved(1).as(i64);a+b
        }
    "#;
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
    assert!(engine.vm_mut().completed_collections() >= before + 5);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}
