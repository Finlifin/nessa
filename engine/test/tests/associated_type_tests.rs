//! Associated types select concrete signatures from the exact implementation.
mod common;

#[test]
fn concrete_methods_substitute_explicit_and_default_associated_types() {
    for source in [
        "struct P{text:String};trait Stream{assoc Item:Type=Any;fn next(self)->?Item};impl Stream for P{assoc Item:Type=String;pub fn next(self)->?String{self.text}};fn main(){let p=P{text:\"abcdefgh\"};let text:String=p.next().as(String);text.len()+34}",
        "struct P{};trait Stream{assoc Item:Type=i64;fn next(self)->?Item};impl Stream for P{pub fn next(self)->?i64{42}};fn main(){let p=P{};let n:i64=p.next().as(i64);n}",
        "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->?Item};impl Stream for P{pub fn next(self)->?Any{42}};fn main(){let p=P{};p.next().as(i64)}",
        "struct P{};trait Stream{assoc Item:Type=Any;derive fn next(self)->?Item{null}};impl Stream for P{assoc Item:Type=String};fn main(){let result:?String=P{}.next();if result==null{42}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn associated_names_are_owned_by_their_trait_and_explicit_any_is_unchanged() {
    for source in [
        "struct P{};trait NumberSource{assoc Item:Type=Any;fn number(self)->Item};trait TextSource{assoc Item:Type=Any;fn text(self)->Item};impl NumberSource for P{assoc Item:Type=i64;pub fn number(self)->i64{42}};impl TextSource for P{assoc Item:Type=String;pub fn text(self)->String{\"abcdefgh\"}};fn main(){let p=P{};let n:i64=p.number();let text:String=p.text();if text.len()==8{n}else{0}}",
        "struct P{};trait Stream{assoc Item:Type=Any;fn pair(self,value:Item,raw:Any)->(Item,Any)};impl Stream for P{assoc Item:Type=String;pub fn pair(self,value:String,raw:Any)->(String,Any){(value,raw)}};fn main(){let p=P{};let result:(String,Any)=p.pair(\"abcdefgh\",34);result.0.len()+result.1.as(i64)}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn scoped_associated_bindings_survive_the_complete_archive_path() {
    let source = "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};mod a{extend Stream for P{assoc Item:Type=String;pub fn next(self)->String{\"abcdefgh\"}};pub fn answer()->i64{let p=P{};let text:String=p.next();text.len()+34}};mod b{extend Stream for P{assoc Item:Type=i64;pub fn next(self)->i64{42}};pub fn answer()->i64{let p=P{};let n:i64=p.next();n}};fn main(){a.answer()+b.answer()-42}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let all_bindings = artifact.type_pool.associated_bindings_snapshot();
    let bindings: Vec<_> = all_bindings
        .iter()
        .filter(|binding| {
            ![
                artifact.type_pool.well_known.iterator,
                artifact.type_pool.well_known.into_iterator,
            ]
            .contains(&binding.trait_owner)
        })
        .collect();
    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0].implementor, bindings[1].implementor);
    assert_eq!(bindings[0].trait_type, bindings[1].trait_type);
    assert_ne!(bindings[0].visible_scope, bindings[1].visible_scope);
    assert!(
        bindings
            .iter()
            .all(|binding| binding.visible_scope.is_some())
    );
    for intrinsic in [type_pool::Intrinsic::Str, type_pool::Intrinsic::I64] {
        assert!(
            bindings
                .iter()
                .any(|binding| artifact.type_pool.as_intrinsic(binding.value) == Some(intrinsic))
        );
    }
    let signature = artifact
        .type_pool
        .trait_schema(bindings[0].trait_type)
        .unwrap()
        .slots[0]
        .signature
        .as_ref()
        .unwrap();
    assert_eq!(
        signature.associated_paths,
        vec![type_pool::TraitAssociatedPath {
            trait_owner: bindings[0].trait_owner,
            name: str_interner::intern("Item"),
            path: vec![type_pool::TraitTypeStep::Return],
        }]
    );
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    assert_eq!(
        restored.type_pool.associated_bindings_snapshot(),
        all_bindings
    );
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), restored)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}

#[test]
fn invalid_bindings_and_dynamic_associated_interfaces_are_rejected() {
    for (source, expected) in [
        (
            "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Missing:Type=i64;pub fn next(self)->i64{42}};fn main(){42}",
            "associated binding does not identify one declaration",
        ),
        (
            "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Item:Type=i64;assoc Item:Type=String;pub fn next(self)->i64{42}};fn main(){42}",
            "duplicate definition",
        ),
        (
            "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Item:Type=42;pub fn next(self)->i64{42}};fn main(){42}",
            "associated binding value must be a statically known",
        ),
        (
            "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Item:Type=i64;pub fn next(self)->String{\"wrong\"}};fn main(){42}",
            "signature mismatch",
        ),
        (
            "trait Stream{assoc Item:Type=Any;fn next(self)->?Item};fn consume(x:Stream){x.next()};fn main(){42}",
            "dynamic trait methods with associated types",
        ),
        (
            "trait Stream{assoc Item:Type=Any;fn next(self)->?Item};fn consume(x:?Stream){42};fn main(){42}",
            "associated",
        ),
        (
            "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->?Item};impl Stream for P{assoc Item:Type=String;pub fn next(self)->?String{null}};fn main(){let p=P{};let x:Stream=p;42}",
            "dynamic trait views with associated types",
        ),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "{source}: {:?}",
            compiled.diagnostics
        );
        assert!(compiled.codegen_output.functions.is_empty(), "{source}");
        assert!(compiled.into_artifact().is_err(), "{source}");
    }
}

fn collect_at_print(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn concrete_associated_returns_remain_rooted_across_collection_and_continuations() {
    for source in [
        "struct P{text:String};trait Stream{assoc Item:Type=Any;fn next(self)->?Item};impl Stream for P{assoc Item:Type=String;pub fn next(self)->?String{print(\"\");self.text}};fn main(){let p=P{text:\"abcd\"++\"efgh\"};let f=||p.next().as(String);print(\"\");let text:String=f();print(\"\");text.len()+34}",
        "struct P{text:String};effect pause(catch k)->i64;trait Stream{assoc Item:Type=Any;fn next(self)->?Item};impl Stream for P{assoc Item:Type=String;pub fn next(self)->?String{pause()#;print(\"\");self.text}};fn main(){let p=P{text:\"abcd\"++\"efgh\"};let saved=p.next()#{pause(k)=>k};print(\"\");let first:String=saved(0).as(String);let second:String=saved(1).as(String);if first.len()==8 and second.len()==8{42}else{0}}",
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
        assert!(engine.vm_mut().completed_collections() >= before + 3);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
