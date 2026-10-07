//! Each concrete associated default body must satisfy its selected bindings.
mod common;

#[test]
fn selected_associated_bindings_check_default_bodies_before_emitting_code() {
    for body in [
        "derive fn result(self)->Item{42}",
        "derive fn result(self)->Item{let copy:Item=42;copy}",
        "derive fn result(self)->fn()->Item{||42}",
        "derive fn result(self)->Item{fn inner()->Item{42};inner()}",
        "derive fn result(self,value:Item)->(Item,Item){(value,42)}",
    ] {
        let source = format!(
            "struct P{{}};trait Source{{assoc Item:Type=Any;{body}}};impl Source for P{{assoc Item:Type=String}};fn main(){{42}}"
        );
        let compiled = driver::Driver::new().compile(&source);
        assert!(
            compiled.has_errors,
            "accepted invalid concrete body: {source}"
        );
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.message.contains("type mismatch") }),
            "no concrete type diagnostic for {source}: {:?}",
            compiled.diagnostics
        );
        assert!(
            compiled.codegen_output.functions.is_empty(),
            "invalid default body emitted code: {source}"
        );
        assert!(compiled.into_artifact().is_err(), "{source}");
    }
}

#[test]
fn a_valid_binding_does_not_hide_an_invalid_second_specialization() {
    let source = "struct P{};struct Q{};trait Source{assoc Item:Type=Any;derive fn result(self)->Item{42}};impl Source for P{assoc Item:Type=i64};impl Source for Q{assoc Item:Type=String};fn main(){P{}.result()}";
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "invalid Q adapter was accepted");
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("type mismatch") }),
        "{:?}",
        compiled.diagnostics
    );
    assert!(compiled.codegen_output.functions.is_empty());
    assert!(compiled.into_artifact().is_err());
}

#[test]
fn an_explicit_override_does_not_instantiate_an_unused_default_body() {
    for source in [
        "struct P{};trait Source{assoc Item:Type=Any;derive fn result(self)->Item{42}};impl Source for P{assoc Item:Type=i64};fn main(){let n:i64=P{}.result();n}",
        "struct P{};trait Source{assoc Item:Type=Any;derive fn result(self)->Item{42}};impl Source for P{assoc Item:Type=String;pub fn result(self)->String{\"abcdefgh\"}};fn main(){let text:String=P{}.result();text.len()+34}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}

#[test]
fn nested_self_parameters_require_a_carrier_before_emitting_an_artifact() {
    for body in [
        "derive fn read(self,pair:(Self,i64))->Item{42}",
        "derive fn callback(self)->fn((Self,i64))->Item{|pair:(Self,i64)|42}",
        "derive fn callback(self)->fn((Self,i64))->Item{fn inner(pair:(Self,i64))->Item{42};inner}",
    ] {
        let source = format!(
            "struct P{{}};trait Source{{assoc Item:Type=i64;{body}}};impl Source for P{{}};fn main(){{42}}"
        );
        let compiled = driver::Driver::new().compile(&source);
        assert!(compiled.has_errors, "{source}");
        assert!(
            compiled.diagnostics.iter().any(|diagnostic| diagnostic
                .message
                .contains("cannot carry Self evidence inside a parameter type")),
            "{source}: {:?}",
            compiled.diagnostics
        );
        assert!(compiled.codegen_output.functions.is_empty());
        assert!(compiled.into_artifact().is_err());
    }
}

#[test]
fn structural_item_parameters_and_self_returns_remain_concrete_values() {
    for source in [
        "struct P{x:i64};trait Source{assoc Item:Type=Self;derive fn mapper(self)->fn((Item,i64))->i64{|pair:(Item,i64)|pair.0.x+pair.1}};impl Source for P{};fn main(){P{x:0}.mapper()((P{x:40},2))}",
        "struct P{x:i64};trait Source{assoc Item:Type=Self;derive fn pair(self)->(Self,i64){(self,2)}};impl Source for P{};fn main(){let pair:(P,i64)=P{x:40}.pair();pair.0.x+pair.1}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42
        );
    }
}
