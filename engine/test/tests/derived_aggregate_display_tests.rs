//! Derived aggregate Display calls real field methods with frame-owned bounds.
mod common;

#[test]
fn enum_and_tuple_display_invoke_fields_and_quote_only_strings() {
    for source in [
        r#"struct P{x:i64};impl Display for P{pub fn to_string(self)->String{"custom"}};enum E{empty,payload(p:P,text:String)};derive Display for E;fn show(x:Display)->String{x.to_string()};fn main(){if show(E.empty)=="E.empty" and show(E.payload(P{x:42},"hello"))=="E.payload(custom, \"hello\")"{42}else{0}}"#,
        r#"struct P{};impl Display for P{pub fn to_string(self)->String{"custom"}};typealias T=(P,String);derive Display for T;fn main(){if (P{},"hello").to_string()=="(custom, \"hello\")"{42}else{0}}"#,
        r#"enum E{payload(x:(i64,(String,)))};derive Display for E;fn main(){if E.payload((42,("hello",))).to_string()=="E.payload((42, (\"hello\",)))"{42}else{0}}"#,
        r#"struct P{};impl Display for P{pub fn to_string(self)->String{self.to_string()}};enum E{payload(x:P)};derive Display for E;fn main(){if E.payload(P{}).to_string()=="E.payload(<cycle>)"{42}else{0}}"#,
        r#"struct P{};trait Style(Display){};impl Display for P{pub fn to_string(self)->String{style(self)}};impl Style for P{pub fn to_string(self)->String{"wrong-unbounded"}};fn style(x:Style)->String{x.to_string()};enum E{payload(x:P)};derive Display for E;fn main(){if E.payload(P{}).to_string()=="E.payload(<cycle>)"{42}else{0}}"#,
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn aggregate_composition_roots_receivers_prefixes_and_multishot_paths() {
    for source in [
        r#"struct P{text:String};impl Display for P{pub fn to_string(self)->String{print("");self.text}};enum E{payload(prefix:String,x:(P,String))};derive Display for E;fn main(){let result=E.payload("prefix"++"-root",(P{text:"abcd"++"efgh"},"suffix"++"-root")).to_string();print("");if result=="E.payload(\"prefix-root\", (abcdefgh, \"suffix-root\"))"{42}else{0}}"#,
        r#"effect pause(catch k)->i64;struct P{text:String};impl Display for P{pub fn to_string(self)->String{pause()#;print("");self.text}};enum E{payload(prefix:String,x:(P,String))};derive Display for E;fn main(){let saved=E.payload("prefix"++"-root",(P{text:"abcd"++"efgh"},"suffix"++"-root")).to_string()# {pause(k)=>k};print("");let first=saved(0);print("");let second=saved(1);if first=="E.payload(\"prefix-root\", (abcdefgh, \"suffix-root\"))" and second==first{42}else{0}}"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(!compiled.has_errors, "{source}: {:?}", compiled.diagnostics);
        let mut engine = initialization::Engine::with_defaults();
        let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
            .unwrap()
            .unwrap();
        engine
            .vm_mut()
            .register_builtin(runtime::ids::PRINT, collect);
        let before = engine.vm_mut().completed_collections();
        let task = engine.vm_mut().spawn_root(entry);
        assert!(
            matches!(engine.vm_mut().run(), interpreter::VmResult::Finished),
            "{source}"
        );
        assert!(engine.vm_mut().completed_collections() >= before + 2);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
