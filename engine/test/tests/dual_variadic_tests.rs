//! Dual source packing retains declaration identity, order, and concrete type facts.
mod common;

use ast::{NodeIndex, NodeKind};
use diagnostic::{DiagnosticContext, Level};
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use type_pool::{Intrinsic, TraitParameterKind, TypeKind};

fn assert_value(source: &str) {
    assert_eq!(common::run_value(source), Ok(42), "{source}");
}

fn rejected(source: &str) {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "accepted {source}");
    assert!(compiled.codegen_output.functions.is_empty(), "{source}");
    assert!(
        compiled.diagnostics.iter().any(|d| d.level == Level::Error),
        "{source}"
    );
    assert!(
        !compiled
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Expected")),
        "a generic parser error does not establish a dual-layout diagnostic: {source}: {:?}",
        compiled.diagnostics
    );
    assert!(
        compiled.diagnostics.iter().any(|d| d.level == Level::Error
            && ["variadic", "extended application", "type mismatch"]
                .iter()
                .any(|category| d.message.contains(category))),
        "missing semantic rejection category: {source}: {:?}",
        compiled.diagnostics
    );
    assert!(compiled.into_artifact().is_err());
}

// The raw resolver does not inject std. Only this facts helper supplies privileged
// canonical collection aliases; execution helpers use the ordinary driver/std path.
fn checked(source: &str) -> resolution::ResolvedAst {
    let source = format!("typealias List=.List'builtin;typealias Map=.Map'builtin;{source}");
    let (tokens, errors) = lexer::tokenize(&source);
    assert!(errors.is_empty(), "{errors:?}");
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("dual.ns".into()), source.clone());
    let diagnostics = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, &source, &diagnostics, file.start_pos).parse();
    let resolved = resolution::resolve_with_options(
        ast,
        &diagnostics,
        resolution::ResolveOptions::for_builtin_package(),
    );
    let mut errors = diagnostics.diagnostics().to_vec();
    errors.extend(resolved.diagnostics.iter().cloned());
    assert!(
        !errors.iter().any(|d| d.level == Level::Error),
        "{source}: {errors:?}"
    );
    resolved
}

fn assert_function_result(resolved: &resolution::ResolvedAst, name: &str, expected: Intrinsic) {
    let symbol = resolved
        .symbols
        .iter()
        .find(|s| str_interner::get(s.name) == name)
        .unwrap();
    let signature = resolved
        .type_pool
        .canonical_type(symbol.type_index)
        .unwrap();
    let TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind else {
        panic!("{name} must remain a function")
    };
    assert_eq!(
        resolved.type_pool.as_intrinsic(ret),
        Some(expected),
        "{name}"
    );
}

#[test]
fn empty_children_properties_and_interleaved_entries_pack_canonical_aliases() {
    for source in [
        "fn gather(...xs:List,...ps:Map)->i64{if xs.len()==0 and ps.len()==0{42}else{0}};fn main(){gather{}}",
        "typealias Children=List;typealias Properties=Map;fn gather(...xs:Children,...ps:Properties)->i64{if xs.len()==2 and ps.len()==0{xs(0).as(i64)+xs(1).as(i64)}else{0}};fn main(){gather{40,2}}",
        "fn gather(...xs:List,...ps:Map)->i64{if xs.len()==0 and ps.len()==2{ps(\"left\").as(i64)+ps(\"right\").as(i64)}else{0}};fn main(){gather{left:40,right:2}}",
        "fn gather(...xs:List,...ps:Map)->i64{if xs.len()==2 and ps.len()==2{xs(0).as(i64)+ps(\"missing_variable\").as(i64)+xs(1).as(i64)+ps(\"tail\").as(i64)}else{0}};fn main(){gather{10,missing_variable:20,10,tail:2}}",
    ] {
        assert_value(source);
    }
    let resolved = checked(
        "typealias Children=List;typealias Properties=Map;fn gather(...xs:Children,...ps:Properties){42};fn main(){gather{40,tag:2,null,tag:()}}",
    );
    let (call, plan) = resolved
        .call_arguments
        .iter()
        .find(|(node, _)| resolved.ast.node(**node).kind == NodeKind::ExtendedCall)
        .unwrap();
    assert_eq!(
        resolved.type_pool.as_intrinsic(resolved.node_types[call]),
        Some(Intrinsic::I64)
    );
    assert_eq!(plan.parameters.len(), 2);
    assert_eq!(
        plan.parameters[0].value,
        resolution::CallArgumentValue::Variadic {
            source_indices: vec![0, 2]
        }
    );
    assert_eq!(
        plan.parameters[1].value,
        resolution::CallArgumentValue::MapVariadic {
            source_properties: vec![
                (str_interner::intern("tag"), 1),
                (str_interner::intern("tag"), 3)
            ]
        }
    );
    for (binding, expected) in plan.parameters.iter().zip([
        resolved.type_pool.list_type().unwrap(),
        resolved.type_pool.map_type().unwrap(),
    ]) {
        assert_eq!(binding.symbols.len(), 1);
        assert_eq!(
            resolved
                .type_pool
                .canonical_type(resolved.symbols[binding.symbols[0].0 as usize].type_index),
            Some(expected)
        );
    }
}

#[test]
fn receiver_and_interleaved_values_evaluate_once_duplicates_evaluate_then_last_wins() {
    assert_value(
        r#"
        global trace:i64=0;struct P{};
        fn receiver()->P{trace=trace*10+1;P{}};
        fn value(n:i64)->i64{trace=trace*10+n;n};
        impl P{pub fn gather(self,...xs:List,...ps:Map)->i64{
            if trace==12345 and xs.len()==2 and ps.len()==1 and xs(0)==2 and xs(1)==4 and ps("same")==5{42}else{0}
        }};
        fn main(){receiver().gather{value(2),same:value(3),value(4),same:value(5)}}
    "#,
    );
    assert_value(
        r#"
        global n:i64=40;
        fn later()->i64{n=0;2};
        fn gather(...xs:List,...ps:Map)->i64{xs(0).as(i64)+ps("tail").as(i64)};
        fn main(){gather{n,tail:later()}}
    "#,
    );
}

#[test]
fn null_unit_narrow_tuples_heap_and_closures_keep_actual_values_and_sharing() {
    assert_value(
        r#"
        struct Cell{n:i64};
        fn gather(...xs:List,...ps:Map)->i64{
            let cell=xs(4).as(Cell);cell.n=cell.n+2;
            let callback=ps("callback").as(fn()->i64);
            if xs.len()==5 and xs(0)==null and xs(1)==() and type_of(xs(2))==i8
                and xs(3).as((i64,i64)).1==2 and ps("cell").as(Cell).n==42{callback()}else{0}
        };
        fn main(){let cell=Cell{n:40};let small:i8=42;gather{null,(),small,(40,2),cell,cell:cell,callback:||cell.n}}
    "#,
    );
}

#[test]
fn module_import_reexports_literal_lambdas_and_static_methods_retain_source_layout() {
    for source in [
        "use api.gather as selected;mod api{pub use implementation.gather};mod implementation{pub fn gather(...xs:List,...ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}};fn main(){selected{40,tail:2}}",
        "fn main(){(|...xs:List,...ps:Map|->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}){40,tail:2}}",
        "struct P{};impl P{pub fn gather(...xs:List,...ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}};fn main(){P.gather{40,tail:2}}",
    ] {
        assert_value(source);
    }
}

#[test]
fn effect_catch_slots_are_injected_at_every_position_and_duplicates_keep_effects() {
    for (parameters, pattern) in [
        ("catch k,...xs:List,...ps:Map", "k,xs,ps"),
        ("...xs:List,catch k,...ps:Map", "xs,k,ps"),
        ("...xs:List,...ps:Map,catch k", "xs,ps,k"),
    ] {
        assert_value(&format!(
            "effect gather({parameters})->i64;fn main(){{gather{{40,tail:2}}#{{gather({pattern})=>k(xs(0).as(i64)+ps(\"tail\").as(i64))}}}}"
        ));
    }
    assert_value(
        "effect gather(...xs:List,...ps:Map)->i64;fn main(){gather{40,tail:2}#{gather(xs,ps)=>xs(0).as(i64)+ps(\"tail\").as(i64)}}",
    );
    assert_value(
        r#"
        global trace:i64=0;effect tick(n:i64)->i64;
        fn value(n:i64)->i64{tick(n)};
        fn gather(...xs:List,...ps:Map)->i64{if trace==123 and ps.len()==1{ps("answer").as(i64)+39}else{0}};
        fn main(){gather{answer:value(1),answer:value(2),answer:value(3)}#{tick(n)=>{trace=trace*10+n;n}}}
    "#,
    );
}

#[test]
fn trait_modes_and_scoped_self_item_defaults_preserve_declaration_and_provider() {
    let source = r#"
        struct P{n:i64};trait Source{
            assoc Item:Type=Self;fn next(self)->Item;
            derive fn gather(self,...xs:List,...ps:Map)->Item{
                let copy:Self=self;let value:Item=copy.next();
                let callback=ps("callback").as(fn(Item)->Item);callback(value)
            }
        };
        impl Source for P{pub fn next(self)->P{self}};
        fn identity(value:P)->P{value};
        fn main(){P{n:42}.gather{0,callback:identity}.n}
    "#;
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let slots = compiled
        .type_pool
        .trait_schemas_snapshot()
        .iter()
        .flat_map(|s| &s.slots);
    let slot = slots
        .into_iter()
        .find(|slot| str_interner::get(slot.name) == "gather")
        .unwrap();
    assert_eq!(
        slot.signature.as_ref().unwrap().parameter_kinds,
        vec![
            TraitParameterKind::Receiver,
            TraitParameterKind::ListVariadic,
            TraitParameterKind::MapVariadic
        ]
    );
    assert_value(source);
    assert_value(
        r#"
        struct P{};trait Source{assoc Item:Type=i64;fn next(self)->Item;
            derive fn gather(self,...xs:List,...ps:Map)->Item{let copy=self;copy.next()}};
        mod a{extend Source for P{pub fn next(self)->i64{40}};pub fn read()->i64{P{}.gather{ignored:1}}};
        mod b{extend Source for P{pub fn next(self)->i64{2}};pub fn read()->i64{P{}.gather{0}}};
        fn main(){a.read()+b.read()}
    "#,
    );
}

#[test]
fn receiver_factories_and_extended_calls_infer_concrete_results_in_both_orders() {
    for declarations in [
        "fn create(...xs:List,...ps:Map){P{}};fn factory(){create{}};fn wrapper(){factory().gather{answer:42}};",
        "fn wrapper(){factory().gather{answer:42}};fn factory(){create{}};fn create(...xs:List,...ps:Map){P{}};",
    ] {
        assert_value(&format!(
            "struct P{{}};impl P{{pub fn gather(self,...xs:List,...ps:Map){{ps(\"answer\").as(i64)}}}};{declarations}fn main(){{wrapper()}}"
        ));
    }
    for reversed in [false, true] {
        let mut definitions = vec!["fn leaf(...xs:List,...ps:Map){true};".to_owned()];
        for index in 0..512 {
            let body = if index == 0 {
                "leaf{answer:42}".to_owned()
            } else {
                format!("chain_{}{{}}", index - 1)
            };
            definitions.push(format!("fn chain_{index}(...xs:List,...ps:Map){{{body}}};"));
        }
        if reversed {
            definitions.reverse();
        }
        let source = format!(
            "{}fn main(){{if (chain_511{{}}){{42}}else{{0}}}}",
            definitions.join("")
        );
        let resolved = checked(&source);
        assert_function_result(&resolved, "leaf", Intrinsic::Bool);
        for index in 0..512 {
            assert_function_result(&resolved, &format!("chain_{index}"), Intrinsic::Bool);
        }
        let mut extended = 0;
        for (index, node) in resolved.ast.nodes.iter().enumerate() {
            if node.kind == NodeKind::ExtendedCall {
                assert_eq!(
                    resolved
                        .type_pool
                        .as_intrinsic(resolved.node_types[&NodeIndex(index as u32)]),
                    Some(Intrinsic::Bool)
                );
                extended += 1;
            }
        }
        assert_eq!(extended, 513);
        assert_value(&source);
        rejected(&format!(
            "{}effect unused(.n:i64=chain_511{{}})->i64;fn main(){{42}}",
            definitions.join("")
        ));
    }
}

#[test]
fn inferred_extended_results_check_selected_and_unused_defaults_and_typed_consumers() {
    for declarations in [
        "fn leaf(...xs:List,...ps:Map){true};fn wrapper(){leaf{}};",
        "fn wrapper(){leaf{}};fn leaf(...xs:List,...ps:Map){true};",
    ] {
        for tail in [
            "effect unused(.n:i64=wrapper())->i64;fn main(){42}",
            "fn consume(n:i64)->i64{n};fn main(){consume(wrapper())}",
        ] {
            rejected(&format!("{declarations}{tail}"));
        }
    }
    let source = "fn gather(...xs:List,...ps:Map){42};fn wrapper(){gather{}};fn main(){wrapper()}";
    let resolved = checked(source);
    assert_function_result(&resolved, "gather", Intrinsic::I64);
    assert_function_result(&resolved, "wrapper", Intrinsic::I64);
    assert_value(source);
}

#[test]
fn large_children_cross_register_window_and_new_list_immediate_limit() {
    for count in [80, 4100] {
        let entries = (0..count)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_value(&format!(
            "fn gather(...xs:List,...ps:Map)->i64{{if xs.len()=={count} and xs(0)==0 and xs({last})=={last} and ps(\"tail\")==2{{xs(40).as(i64)+ps(\"tail\").as(i64)}}else{{0}}}};fn main(){{gather{{{entries},tail:2}}}}",
            last = count - 1
        ));
    }
}

#[test]
fn initialization_executes_packed_child_and_property_callbacks_before_consumers() {
    for (body, entries) in [
        ("let f=xs(0).as(fn()->i64);f()", "provider.read"),
        (
            "let f=ps(\"callback\").as(fn()->i64);f()",
            "callback:provider.read",
        ),
    ] {
        assert_value(&format!(
            "fn invoke(...xs:List,...ps:Map)->i64{{{body}}};mod consumer{{pub global answer:i64=invoke{{{entries}}}}};mod provider{{pub global value:i64=42;pub fn read()->i64{{value}}}};fn main(){{consumer.answer}}"
        ));
    }
    assert_value(
        "fn ignore(...xs:List,...ps:Map)->i64{42};mod consumer{pub global answer:i64=ignore{unused.read,callback:unused.read}};mod unused{pub global value:i64=consumer.answer;pub fn read()->i64{value}};fn main(){consumer.answer}",
    );
    assert_value(
        "fn invoke(...xs:List,...ps:Map)->i64{let f=ps(\"callback\").as(fn()->i64);f()};mod consumer{pub global answer:i64=invoke{callback:unused.read,callback:provider.read}};mod unused{pub global value:i64=consumer.answer;pub fn read()->i64{value}};mod provider{pub global value:i64=42;pub fn read()->i64{value}};fn main(){consumer.answer}",
    );
    assert_value(
        "struct P{};trait Source{fn next(self)->i64;derive fn gather(self,...xs:List,...ps:Map)->i64{let copy=self;copy.next()}};mod api{extend Source for P{pub fn next(self)->i64{storage.value}};pub global answer:i64=P{}.gather{}};mod unused{pub global value:i64=api.answer;extend Source for P{pub fn next(self)->i64{value}}};mod storage{pub global value:i64=42};fn main(){api.answer}",
    );
}

#[test]
fn invalid_unused_layouts_and_defaults_reject_without_artifacts() {
    for declaration in [
        "fn bad(...xs:Map,...ps:List){42}",
        "fn bad(...xs:List,...ps:List){42}",
        "fn bad(n:i64,...xs:List,...ps:Map){42}",
        "fn bad(...xs:List,...ps:Map,.n:i64=42){42}",
        "fn bad(...xs:List,...ps:Map,...other:List){42}",
        "fn bad(...xs:Map){42}",
        "fn bad(...xs:i64,...ps:Map){42}",
        "effect bad(...xs:List,...ps:Map,.n:i64=true)->i64",
        "enum E{bad(...xs:List,...ps:Map)}",
        "trait T{fn bad(self,n:i64,...xs:List,...ps:Map)->i64}",
    ] {
        rejected(&format!("{declaration};fn main(){{42}}"));
    }
}

#[test]
fn ordinary_dual_calls_and_function_values_do_not_invent_source_packing_metadata() {
    for main in [
        "fn main(){gather([],Map())}",
        "fn main(){gather()}",
        "fn main(){let f=gather;f{40,tail:2}}",
        "fn main(){let f:fn(List,Map)->i64=gather;f{40,tail:2}}",
        "fn main(){let f=|...xs:List,...ps:Map|->i64{42};f{}}",
    ] {
        rejected(&format!(
            "fn gather(...xs:List,...ps:Map)->i64{{42}};{main}"
        ));
    }
    rejected("fn use_value(f:fn(List,Map)->i64)->i64{f{}};fn main(){42}");
    for declaration in [
        "fn gather(xs:List,ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}",
        "fn gather(...xs:List,...ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}",
    ] {
        assert_value(&format!(
            "{declaration};fn main(){{let f:fn(List,Map)->i64=gather;let ps=Map();ps(\"tail\")=2;f([40],ps)}}"
        ));
    }
}

#[test]
fn existing_structs_single_list_enum_variadics_and_ordinary_calls_are_preserved() {
    for source in [
        "struct P{left:i64,right:i64=2};fn main(){P{left:40}.left+P{left:0}.right}",
        "fn gather(prefix:i64,...xs:List,.tail:i64=xs.len())->i64{prefix+xs(0).as(i64)+tail};fn main(){gather(39,2)}",
        "enum E{values(...xs:List)};fn main(){E.values(40,2) match{E.values(xs)=>xs(0).as(i64)+xs(1).as(i64)}}",
        "fn add(a:i64,b:i64)->i64{a+b};fn main(){add(40,2)}",
    ] {
        assert_value(source);
    }
}

#[test]
fn trait_receiver_calls_and_factory_calls_keep_concrete_signatures() {
    assert_value(
        "struct P{};trait Gather{fn gather(self,...xs:List,...ps:Map)->i64};impl Gather for P{pub fn gather(self,...xs:List,...ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}};fn call(value:Gather)->i64{value.gather{40,tail:2}};fn main(){call(P{})}",
    );
    for declarations in [
        "fn create(...xs:List,...ps:Map){P{}};fn factory(){create{}};fn wrapper(){factory().gather{answer:42}};",
        "fn wrapper(){factory().gather{answer:42}};fn factory(){create{}};fn create(...xs:List,...ps:Map){P{}};",
    ] {
        for (body, expected, main) in [
            ("true", Intrinsic::Bool, "if wrapper(){42}else{0}"),
            ("42", Intrinsic::I64, "wrapper()"),
        ] {
            let source = format!(
                "struct P{{}};impl P{{pub fn gather(self,...xs:List,...ps:Map){{{body}}}}};{declarations}fn main(){{{main}}}"
            );
            let resolved = checked(&source);
            assert_function_result(&resolved, "gather", expected);
            assert_function_result(&resolved, "wrapper", expected);
            for name in ["create", "factory"] {
                let symbol = resolved
                    .symbols
                    .iter()
                    .find(|s| str_interner::get(s.name) == name)
                    .unwrap();
                let signature = resolved
                    .type_pool
                    .canonical_type(symbol.type_index)
                    .unwrap();
                let TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind else {
                    panic!("missing function")
                };
                let ret = resolved.type_pool.canonical_type(ret).unwrap();
                assert!(
                    matches!(&resolved.type_pool.get(ret).kind,TypeKind::Struct{name,..} if str_interner::get(*name)=="P")
                );
            }
            let mut receiver_nodes = 0;
            let mut result_nodes = 0;
            for (index, node) in resolved.ast.nodes.iter().enumerate() {
                if node.kind == NodeKind::ExtendedCall {
                    let ty = resolved
                        .type_pool
                        .canonical_type(resolved.node_types[&NodeIndex(index as u32)])
                        .unwrap();
                    if resolved.type_pool.as_intrinsic(ty) == Some(expected) {
                        result_nodes += 1
                    } else {
                        assert!(
                            matches!(&resolved.type_pool.get(ty).kind,TypeKind::Struct{name,..} if str_interner::get(*name)=="P")
                        );
                        receiver_nodes += 1;
                    }
                }
            }
            assert_eq!((receiver_nodes, result_nodes), (2, 1));
            assert_value(&source);
        }
    }
    assert_value(
        "fn gather(...xs:List,...ps:Map)->i8{42};fn main(){let value=gather{};if type_of(value)==i8{value.as(i64)}else{0}}",
    );
}

#[test]
fn packing_explicit_any_payloads_does_not_recover_erased_scoped_trait_authority() {
    for (entry, body) in [
        ("erased", "let value=xs(0);consume(value)"),
        ("value:erased", "let value=ps(\"value\");consume(value)"),
    ] {
        let source = format!(
            "struct P{{}};trait Read{{fn value(self)->i64}};mod a{{extend Read for P{{pub fn value(self)->i64{{42}}}};pub fn answer()->i64{{outside.take(P{{}})}}}};mod outside{{fn consume(x:Read)->i64{{x.value()}};fn pack(...xs:List,...ps:Map)->i64{{{body}}};pub fn take(x:Read)->i64{{let erased:Any=x;pack{{{entry}}}}}}};fn main(){{a.answer()}}"
        );
        assert!(
            matches!(common::run_value(&source),Err(error) if error.contains("TypeError")),
            "{source}"
        );
    }
}
