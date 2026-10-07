//! Dependent associated defaults specialize the exact concrete implementation.
mod common;

#[test]
fn self_defaults_preserve_each_concrete_return_type_and_alias() {
    for source in [
        "struct P{x:i64};struct Q{x:i64};trait Copy{assoc Item:Type=Self;fn copy(self)->Item};impl Copy for P{pub fn copy(self)->P{self}};impl Copy for Q{pub fn copy(self)->Q{self}};fn main(){let p:P=P{x:40}.copy();let q:Q=Q{x:2}.copy();if type_of(p)==P and type_of(q)==Q{p.x+q.x}else{0}}",
        "struct P{x:i64};typealias Alias=P;trait Copy{assoc typealias Owner=Self;assoc Item:Type=Owner;fn copy(self)->Item};impl Copy for Alias{pub fn copy(self)->Alias{self}};fn main(){let p:Alias=Alias{x:42}.copy();if type_of(p)==P{p.x}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn forward_dependencies_optional_tuple_and_function_defaults_are_specialized() {
    let source = "struct P{text:String};typealias ConcretePair=(P,?P);trait Source{assoc Callback:Type=fn()->Item;assoc Pair:Type=(Item,?Item);assoc Item:Type=Self;fn pair(self)->Pair;fn callback(self)->Callback};impl Source for P{pub fn pair(self)->(P,?P){(self,self)};pub fn callback(self)->fn()->P{||self}};fn main(){let p=P{text:\"abcdefgh\"};let pair:(P,?P)=p.pair();let f:fn()->P=p.callback();if type_of(f)==fn()->P and type_of(pair)==ConcretePair{pair.0.text.len()+pair.1.as(P).text.len()+f().text.len()+18}else{0}}";
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        42
    );
}

#[test]
fn function_default_parameter_and_return_both_use_the_selected_item() {
    let source = "struct P{x:i64};trait Source{assoc Mapper:Type=fn(Item)->Item;assoc Item:Type=Self;fn mapper(self)->Mapper};impl Source for P{pub fn mapper(self)->fn(P)->P{|other:P|other}};fn main(){let f:fn(P)->P=P{x:0}.mapper();let p:P=f(P{x:42});if type_of(f)==fn(P)->P and type_of(p)==P{p.x}else{0}}";
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        42
    );
}

#[test]
fn explicit_bindings_override_defaults_before_resolving_other_defaults() {
    for source in [
        "struct P{};trait Source{assoc Pair:Type=(Item,?Item);assoc Item:Type=Self;fn pair(self)->Pair};impl Source for P{assoc Item:Type=String;pub fn pair(self)->(String,?String){(\"abcdefgh\",\"abcdefgh\")}};fn main(){let result:(String,?String)=P{}.pair();result.0.len()+result.1.as(String).len()+26}",
        "struct P{};trait Source{assoc A:Type=B;assoc B:Type=A;fn value(self)->A};impl Source for P{assoc B:Type=i64;pub fn value(self)->i64{42}};fn main(){let n:i64=P{}.value();n}",
        "struct P{};trait Source{assoc First:Type=Any;assoc Second:Type=(First,Any);fn pair(self)->Second};impl Source for P{assoc First:Type=String;pub fn pair(self)->(String,Any){(\"abcdefgh\",34)}};fn main(){let pair:(String,Any)=P{}.pair();pair.0.len()+pair.1.as(i64)}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn inherited_and_scoped_dependencies_survive_archive_installation() {
    let source = "struct P{};trait Base{assoc Item:Type=Self;fn item(self)->Item};trait Child(Base){assoc Pair:Type=(Item,i64);fn pair(self)->Pair};mod a{extend Base for P{assoc Item:Type=String;pub fn item(self)->String{\"abcdefgh\"}};extend Child for P{pub fn item(self)->String{\"abcdefgh\"};pub fn pair(self)->(String,i64){(\"abcdefgh\",34)}};pub fn answer()->i64{let result:(String,i64)=P{}.pair();result.0.len()+result.1}};mod b{extend Base for P{assoc Item:Type=i64;pub fn item(self)->i64{40}};extend Child for P{pub fn item(self)->i64{40};pub fn pair(self)->(i64,i64){(40,2)}};pub fn answer()->i64{let result:(i64,i64)=P{}.pair();result.0+result.1}};fn main(){a.answer()+b.answer()-42}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{source}: {:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let bindings = artifact.type_pool.associated_bindings_snapshot().to_vec();
    let defaults = artifact.type_pool.associated_defaults_snapshot().to_vec();
    assert_eq!(
        defaults
            .iter()
            .filter(|default| ![
                artifact.type_pool.well_known.iterator,
                artifact.type_pool.well_known.into_iterator
            ]
            .contains(&default.trait_owner))
            .count(),
        2
    );
    let parent = defaults.iter().find(|default| matches!(default.expression,
        type_pool::AssociatedTypeExpr::SelfType { trait_owner } if trait_owner == default.trait_owner)).unwrap();
    let child = defaults
        .iter()
        .find(|default| {
            matches!(
                default.expression,
                type_pool::AssociatedTypeExpr::Tuple { .. }
            )
        })
        .unwrap();
    assert_eq!(
        child.expression,
        type_pool::AssociatedTypeExpr::Tuple {
            elements: vec![
                type_pool::AssociatedTypeExpr::Binding {
                    trait_owner: parent.trait_owner,
                    name: parent.name
                },
                type_pool::AssociatedTypeExpr::Concrete(
                    artifact.type_pool.intrinsic(type_pool::Intrinsic::I64)
                ),
            ]
        }
    );
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    assert_eq!(restored.type_pool.associated_bindings_snapshot(), bindings);
    assert_eq!(restored.type_pool.associated_defaults_snapshot(), defaults);
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
}

#[test]
fn an_uninstalled_cycle_can_be_broken_by_a_future_implementation() {
    let source = "trait Source{assoc A:Type=B;assoc B:Type=A};fn main(){42}";
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        42
    );
}

#[test]
fn invalid_dependency_graphs_and_unbound_dynamic_views_emit_no_artifact() {
    for source in [
        "struct P{};trait Source{assoc A:Type=B;assoc B:Type=A;fn value(self)->A};impl Source for P{pub fn value(self)->i64{42}};fn main(){42}",
        "trait Source{assoc Item:Type=Source;fn item(self)->Item};fn main(){42}",
        "struct P{};trait Source{assoc Item:Type=Self;fn item(self)->Item};impl Source for P{pub fn item(self)->String{\"wrong\"}};fn main(){42}",
        "struct P{};trait Source{assoc Item:Type=Self;fn item(self)->Item};impl Source for P{pub fn item(self)->P{self}};fn main(){let x:Source=P{};42}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("associated")
                    || diagnostic.message.contains("signature")),
            "{source}: {:?}",
            compiled.diagnostics
        );
        assert!(compiled.codegen_output.functions.is_empty());
        assert!(compiled.into_artifact().is_err());
    }
}

#[test]
fn excessive_forward_default_dependencies_are_diagnosed_before_codegen() {
    let mut source = String::from("struct P{};trait Source{");
    for index in 0..300 {
        source.push_str(&format!("assoc T{index}:Type=T{};", index + 1));
    }
    source.push_str("assoc T300:Type=i64;fn value(self)->T0};impl Source for P{pub fn value(self)->i64{42}};fn main(){P{}.value()}");
    let compiled = driver::Driver::new().compile(&source);
    assert!(compiled.has_errors);
    assert!(
        compiled.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("associated default")
            && diagnostic.message.contains("exceeds")),
        "{:?}",
        compiled.diagnostics
    );
    assert!(compiled.codegen_output.functions.is_empty());
    assert!(compiled.into_artifact().is_err());
}

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn specialized_self_returns_keep_heap_fields_alive_in_closures_and_multishot_frames() {
    for source in [
        "struct P{text:String};trait Source{assoc Item:Type=Self;assoc Callback:Type=fn()->Item;fn callback(self)->Callback};impl Source for P{pub fn callback(self)->fn()->P{||{print(\"\");self}}};fn main(){let f:fn()->P=P{text:\"abcd\"++\"efgh\"}.callback();print(\"\");let p:P=f();print(\"\");if type_of(f)==fn()->P and type_of(p)==P{p.text.len()+34}else{0}}",
        "struct P{text:String};typealias ConcretePair=(P,?P);effect pause(catch k)->i64;trait Source{assoc Item:Type=Self;assoc Pair:Type=(Item,?Item);fn pair(self)->Pair};impl Source for P{pub fn pair(self)->(P,?P){pause()#;print(\"\");(self,self)}};fn main(){let saved=P{text:\"abcd\"++\"efgh\"}.pair()#{pause(k)=>k};print(\"\");let first:(P,?P)=saved(0).as((P,?P));print(\"\");let second:(P,?P)=saved(1).as((P,?P));if type_of(first)==ConcretePair and type_of(second)==ConcretePair{first.0.text.len()+first.1.as(P).text.len()+second.0.text.len()+second.1.as(P).text.len()+10}else{0}}",
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
        assert!(engine.vm_mut().completed_collections() >= before + 3);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
