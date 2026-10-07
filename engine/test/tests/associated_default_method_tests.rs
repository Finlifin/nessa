//! Associated default bodies execute concrete adapters without erasing Self evidence.
mod common;

#[test]
fn annotations_type_values_and_casts_use_the_exact_item_binding() {
    for source in [
        "enum E{some(value:i64),none};struct P{value:E};trait Source{assoc Item:Type=E;fn next(self)->Item;derive fn answer(self)->i64{self.next() match {E.some(n)=>n,E.none=>0}}};impl Source for P{pub fn next(self)->E{self.value}};fn main(){P{value:E.some(42)}.answer()}",
        "struct P{x:i64};struct Q{x:i64};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn copy(self)->Item{let value:Item=self.next();value.as(Item)};derive fn item_type(self)->Type{Item}};impl Source for P{pub fn next(self)->P{self}};impl Source for Q{pub fn next(self)->Q{self}};fn main(){let p:P=P{x:40}.copy();let q:Q=Q{x:2}.copy();if p.item_type()==P and q.item_type()==Q and type_of(p)==P and type_of(q)==Q{p.x+q.x}else{0}}",
        "struct P{text:String};trait Source{assoc Item:Type=Any;fn next(self)->Item;derive fn copy(self,raw:Any)->(Item,Any){let x:Item=self.next();(x.as(Item),raw)};derive fn item_type(self)->Type{Item}};impl Source for P{assoc Item:Type=String;pub fn next(self)->String{self.text}};fn main(){let p=P{text:\"abcdefgh\"};let result:(String,Any)=p.copy(34);if p.item_type()==String{result.0.len()+result.1.as(i64)}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn nested_closures_named_functions_and_optional_tuples_have_concrete_signatures() {
    for source in [
        "struct P{x:i64};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn callback(self)->fn()->Item{let value:Item=self.next();||->Item{value}};derive fn mapper(self)->fn(Item)->Item{fn map(value:Item)->Item{value};map}};impl Source for P{pub fn next(self)->P{self}};fn main(){let p=P{x:42};let f:fn()->P=p.callback();let map:fn(P)->P=p.mapper();if type_of(f)==fn()->P and type_of(map)==fn(P)->P{map(f()).x}else{0}}",
        "struct P{x:i64};typealias ConcretePair=(P,?P);trait Source{assoc Item:Type=Self;assoc Pair:Type=(Item,?Item);fn next(self)->Item;derive fn pair(self)->Pair{let value:Item=self.next();(value,value)}};impl Source for P{pub fn next(self)->P{self}};fn main(){let pair:ConcretePair=P{x:21}.pair();if type_of(pair)==ConcretePair{pair.0.x+pair.1.as(P).x}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn item_equal_to_self_is_a_value_parameter_and_declared_self_retains_evidence() {
    let source = "struct P{x:i64};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn choose(self,other:Item)->Item{other};derive fn self_next(self,other:Self)->Item{other.next()}};impl Source for P{pub fn next(self)->P{self}};fn main(){let p=P{x:0};p.choose(P{x:40}).x+p.self_next(P{x:2}).x}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let snapshot = compiled.type_pool.snapshot();
    let view=type_pool::TypeIndex::from_raw(snapshot.types.iter().position(|info|
        matches!(&info.kind,type_pool::TypeKind::Trait{name,..} if str_interner::get(*name)=="Source")).unwrap() as u32);
    for (name, expected) in [
        (
            "choose",
            vec![
                nsbc::ParameterAbi::TraitSelf { view },
                nsbc::ParameterAbi::Value,
            ],
        ),
        (
            "self_next",
            vec![
                nsbc::ParameterAbi::TraitSelf { view },
                nsbc::ParameterAbi::TraitSelf { view },
            ],
        ),
    ] {
        let record = compiled
            .type_pool
            .trait_impls_snapshot()
            .iter()
            .find(|record| record.trait_type == view)
            .unwrap();
        let method = record
            .methods
            .iter()
            .find(|method| str_interner::get(method.name) == name)
            .unwrap();
        let function = compiled
            .codegen_output
            .functions
            .iter()
            .find(|function| function.func_id.0 == method.func_id)
            .unwrap();
        assert_eq!(function.abi.as_ref().unwrap().parameters, expected);
    }
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn scoped_binding_and_inherited_override_are_not_reselected_in_the_default_body() {
    for source in [
        "struct P{};trait Source{assoc Item:Type=Any;fn next(self)->Item;derive fn copy(self)->Item{let x:Item=self.next();x};derive fn callback(self)->fn()->Item{||self.copy()}};mod a{extend Source for P{assoc Item:Type=String;pub fn next(self)->String{\"abcdefgh\"}};pub fn callback()->fn()->String{P{}.callback()}};mod b{extend Source for P{assoc Item:Type=i64;pub fn next(self)->i64{42}};pub fn answer()->i64{let f:fn()->String=a.callback();f().len()+P{}.copy()-8}};fn main(){b.answer()}",
        "struct P{};trait Base{assoc Item:Type=i64;fn value(self)->Item;derive fn copy(self)->Item{self.value()}};trait Child(Base){derive fn answer(self)->i64{self.copy()+2}};impl Base for P{pub fn value(self)->i64{2}};impl Child for P{pub fn value(self)->i64{40}};fn main(){P{}.answer()}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn nested_source_self_parameters_keep_their_own_frozen_proof_and_item_parameters_stay_values() {
    let source = "struct P{x:i64};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn callback(self)->fn(Self)->i64{|other:Self|self.next().x+other.next().x};derive fn mapper(self)->fn(Item)->Item{|other:Item|other}};mod a{extend Source for P{pub fn next(self)->P{P{x:40}}};pub fn callback()->fn(P)->i64{P{x:0}.callback()};pub fn mapper()->fn(P)->P{P{x:0}.mapper()}};mod b{extend Source for P{pub fn next(self)->P{P{x:2}}};pub fn answer()->i64{let callback=a.callback();let mapper=a.mapper();if mapper(P{x:7}).x==7{callback(P{x:0})}else{0}}};fn main(){b.answer()}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let view=type_pool::TypeIndex::from_raw(compiled.type_pool.snapshot().types.iter().position(|info|
        matches!(&info.kind,type_pool::TypeKind::Trait{name,..} if str_interner::get(*name)=="Source")).unwrap() as u32);
    assert!(
        compiled
            .codegen_output
            .functions
            .iter()
            .any(|function| function.is_closure
                && function.abi.as_ref().is_some_and(|abi| abi.parameters
                    == [nsbc::ParameterAbi::TraitSelf { view }]
                    && abi
                        .captures
                        .contains(&nsbc::CaptureAbi::TraitProof { view })))
    );
    let p=type_pool::TypeIndex::from_raw(compiled.type_pool.snapshot().types.iter().position(|info|
        matches!(&info.kind,type_pool::TypeKind::Struct{name,..} if str_interner::get(*name)=="P")).unwrap() as u32);
    assert!(compiled.codegen_output.functions.iter().any(|function|function.is_closure &&
        matches!(&compiled.type_pool.get(function.function_type).kind,type_pool::TypeKind::Function{params,ret} if params==&[p] && *ret==p)
        && function.abi.as_ref().is_some_and(|abi|abi.parameters==[nsbc::ParameterAbi::Value])));
    assert_eq!(common::run_value(source).unwrap(), 42);
    let named = "struct P{x:i64};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn named(self)->fn(Self)->Item{fn inner(other:Self)->Item{other.next()};inner}};mod a{extend Source for P{pub fn next(self)->P{P{x:40}}};pub fn callback()->fn(P)->P{P{x:0}.named()}};mod b{extend Source for P{pub fn next(self)->P{P{x:2}}};pub fn answer()->i64{let f=a.callback();f(P{x:0}).x+40}};fn main(){b.answer()}";
    assert_eq!(common::run_value(named).unwrap(), 42);
    let named = driver::Driver::new().compile(named);
    assert!(!named.has_errors, "{:?}", named.diagnostics);
    assert!(named.codegen_output.functions.iter().any(|function| {
        str_interner::get(function.name) == "inner"
            && function
                .abi
                .as_ref()
                .is_some_and(|abi| abi.parameters == [nsbc::ParameterAbi::TraitSelf { view }])
    }));
    let contextual = source.replace("|other:Self|", "|other|");
    assert_eq!(common::run_value(&contextual).unwrap(), 42);
    let contextual = driver::Driver::new().compile(&contextual);
    assert!(!contextual.has_errors, "{:?}", contextual.diagnostics);
    assert!(contextual.codegen_output.functions.iter().any(|function| {
        function.is_closure
            && function
                .abi
                .as_ref()
                .is_some_and(|abi| abi.parameters == [nsbc::ParameterAbi::TraitSelf { view }])
    }));
}

fn collect(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

#[test]
fn specialized_captures_and_paused_defaults_remain_rooted_across_real_collections() {
    for source in [
        "struct P{text:String};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn callback(self)->fn()->Item{let item:Item=self.next();||->Item{print(\"\");item}}};impl Source for P{pub fn next(self)->P{self}};fn main(){let f:fn()->P=P{text:\"abcd\"++\"efgh\"}.callback();print(\"\");let p:P=f();print(\"\");if type_of(f)==fn()->P and type_of(p)==P{p.text.len()+34}else{0}}",
        "struct P{text:String};typealias ConcretePair=(P,?P);effect pause(catch k)->i64;trait Source{assoc Item:Type=Self;assoc Pair:Type=(Item,?Item);fn next(self)->Item;derive fn pair(self)->Pair{let item:Item=self.next();pause()#;print(\"\");(item,item)}};impl Source for P{pub fn next(self)->P{self}};fn main(){let saved=P{text:\"abcd\"++\"efgh\"}.pair()#{pause(k)=>k};print(\"\");let first:ConcretePair=saved(0).as(ConcretePair);print(\"\");let second:ConcretePair=saved(1).as(ConcretePair);if type_of(first)==ConcretePair and type_of(second)==ConcretePair{first.0.text.len()+first.1.as(P).text.len()+second.0.text.len()+second.1.as(P).text.len()+10}else{0}}",
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

#[test]
fn initialization_follows_the_frozen_default_implementation_and_overrides() {
    for source in [
        "mod api{pub global answer:i64=P{}.copy()};mod storage{pub global value:i64=42};struct P{};trait Source{assoc Item:Type=i64;fn next(self)->Item;derive fn copy(self)->Item{self.next()}};impl Source for P{pub fn next(self)->i64{storage.value}};fn main(){api.answer}",
        "mod api{pub global answer:i64=P{}.answer()};mod storage{pub global value:i64=42};mod wrong{pub global value:i64=api.answer};struct P{};trait Base{assoc Item:Type=i64;fn value(self)->Item;derive fn copy(self)->Item{self.value()}};trait Child(Base){derive fn answer(self)->i64{self.copy()}};impl Base for P{pub fn value(self)->i64{wrong.value}};impl Child for P{pub fn value(self)->i64{storage.value}};fn main(){api.answer}",
        "mod api{pub global answer:i64=P{}.copy()};mod storage{pub global value:i64=42};mod unused{pub global value:i64=api.answer};struct P{};trait Source{assoc Item:Type=i64;fn next(self)->Item;derive fn copy(self)->Item{self.next()}};impl Source for P{pub fn next(self)->i64{unused.value};pub fn copy(self)->i64{storage.value}};fn main(){api.answer}",
        "struct P{};trait Source{assoc Item:Type=i64;fn next(self)->Item;derive fn copy(self)->Item{self.next()}};mod a{extend Source for P{pub fn next(self)->i64{storage_a.value}};pub global answer:i64=P{}.copy()};mod b{extend Source for P{pub fn next(self)->i64{storage_b.value}};pub global answer:i64=P{}.copy()};mod storage_a{pub global value:i64=40};mod storage_b{pub global value:i64=2};fn main(){a.answer+b.answer}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
    let forward = "global first:i64=P{}.copy();global later:i64=42;struct P{};trait Source{assoc Item:Type=i64;fn next(self)->Item;derive fn copy(self)->Item{self.next()}};impl Source for P{pub fn next(self)->i64{later}};fn main(){first}";
    assert!(
        common::run_value(forward)
            .unwrap_err()
            .contains("UninitializedGlobal")
    );
}
