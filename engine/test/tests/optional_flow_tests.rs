//! Optional control flow preserves precise payload types and callable boundaries.

mod common;

use ast::{NodeIndex, NodeKind};
use diagnostic::{Diagnostic, DiagnosticContext, Level};
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

fn resolve(source: &str) -> (resolution::ResolvedAst, Vec<Diagnostic>) {
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty(), "{errors:?}");
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(FileName::Custom("optional.ns".into()), source.to_owned());
    let diagnostics = DiagnosticContext::new(&map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    let resolved = resolution::resolve(ast, &diagnostics);
    let mut errors = diagnostics.diagnostics().to_vec();
    errors.extend(resolved.diagnostics.iter().cloned());
    (resolved, errors)
}

fn checked(source: &str) -> resolution::ResolvedAst {
    let (resolved, diagnostics) = resolve(source);
    assert!(
        !diagnostics.iter().any(|d| d.level == Level::Error),
        "{source}: {diagnostics:?}"
    );
    resolved
}

fn result_type(resolved: &resolution::ResolvedAst, name: &str) -> TypeIndex {
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
        panic!("{name} must be a function");
    };
    ret
}

fn rejected_with_message(source: &str, message: &str) {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "accepted {source}");
    assert!(compiled.codegen_output.functions.is_empty());
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.level == Level::Error && d.message.contains(message)),
        "{source}: {:?}",
        compiled.diagnostics
    );
    assert!(compiled.into_artifact().is_err());
}

fn assert_value(source: &str) {
    assert_eq!(common::run_value(source), Ok(42), "{source}");
}

#[test]
fn propagation_extracts_inner_facts_and_infers_implicit_null_returns() {
    let source =
        "fn get()->?i64{42};fn read(){let value=get()?;value};fn main(){if read()==42{42}else{0}}";
    let resolved = checked(source);
    let ret = resolved
        .type_pool
        .canonical_type(result_type(&resolved, "read"))
        .unwrap();
    let TypeKind::Optional { inner } = resolved.type_pool.get(ret).kind else {
        panic!("implicit null exit must remain Optional")
    };
    assert_eq!(resolved.type_pool.as_intrinsic(inner), Some(Intrinsic::I64));
    let mut propagations = 0;
    for (index, node) in resolved.ast.nodes.iter().enumerate() {
        if node.kind == NodeKind::OptionPropagation {
            assert_eq!(
                resolved
                    .type_pool
                    .as_intrinsic(resolved.node_types[&NodeIndex(index as u32)]),
                Some(Intrinsic::I64)
            );
            propagations += 1;
        }
    }
    assert_eq!(propagations, 1);
    assert_value(source);
}

#[test]
fn propagation_preserves_aliases_narrow_values_tuples_heap_and_function_payloads() {
    for source in [
        "typealias Small=i8;typealias Maybe=?Small;fn get()->Maybe{42};fn read()->?i64{let value=get()?;if type_of(value)==i8{value}else{0}};fn main(){if read()==42{42}else{0}}",
        "fn get()->?(i64,i64){(40,2)};fn read()->?i64{let pair=get()?;pair.0+pair.1};fn main(){if read()==42{42}else{0}}",
        "struct P{text:String};fn get()->?P{P{text:\"ab\"++\"cdefgh\"}};fn read()->?i64{get()?.text.len()+34};fn main(){if read()==42{42}else{0}}",
        "fn get()->?fn()->i64{||42};fn read()->?i64{let callback=get()?;callback()};fn main(){if read()==42{42}else{0}}",
        "struct Node{next:?Node,n:i64};fn get()->?Node{Node{next:Node{next:null,n:42},n:0}};fn read()->?i64{get()?.next?.n};fn main(){if read()==42{42}else{0}}",
    ] {
        assert_value(source);
    }
}

#[test]
fn operand_runs_once_and_null_returns_before_remaining_code() {
    for source in [
        "global calls:i64=0;fn get()->?i64{calls+=1;42};fn read()->?i64{let value=get()?;value};fn main(){let value=read();if calls==1 and value==42{42}else{0}}",
        "global trace:i64=0;fn get()->?i64{trace=trace*10+1;null};fn read()->?i64{let value=get()?;trace=trace*10+2;value};fn main(){let value=read();if value==null and trace==1{42}else{0}}",
        "struct P{n:i64};global calls:i64=0;fn get()->?P{calls+=1;P{n:42}};fn read()->?i64{let value=get()?;value.n};fn main(){let value=read();if calls==1 and value==42{42}else{0}}",
    ] {
        assert_value(source);
    }
}

#[test]
fn propagation_returns_from_nested_lambda_and_current_handler_only() {
    for source in [
        "global trace:i64=0;fn outer()->i64{let f=||->?i64{let value:?i64=null;value?;trace=99;42};let result=f();trace+=1;if result==null and trace==1{42}else{0}};fn main(){outer()}",
        "fn outer()->?i64{fn inner()->?i64{let value:?i64=null;value?};let result=inner();if result==null{42}else{0}};fn main(){if outer()==42{42}else{0}}",
        "effect get()->?i64;fn main(){let value=get()#{get()=>{let inner:?i64=null;inner?;42}};if value==null{42}else{0}}",
        "effect pause(catch k)->i64;fn work()->i64{pause()#;42};fn main(){let value=work()#{pause(k)=>{let inner:?i64=null;inner?;0}};if value==null{42}else{0}}",
    ] {
        assert_value(source);
    }
}

#[test]
fn any_operands_keep_gradual_payload_checks_and_null_control() {
    for source in [
        "fn get()->Any{42};fn read()->?i64{let value:i64=get()?;value};fn main(){if read()==42{42}else{0}}",
        "fn get()->Any{null};fn read()->?i64{let value:i64=get()?;value};fn main(){if read()==null{42}else{0}}",
    ] {
        assert_value(source);
    }
    let source =
        "fn get()->Any{true};fn read()->?i64{let value:i64=get()?;value};fn main(){read();42}";
    let resolved = checked(source);
    for (index, node) in resolved.ast.nodes.iter().enumerate() {
        if node.kind == NodeKind::OptionPropagation {
            assert_eq!(
                resolved
                    .type_pool
                    .as_intrinsic(resolved.node_types[&NodeIndex(index as u32)]),
                Some(Intrinsic::Any)
            );
        }
    }
    assert!(common::run_value(source).unwrap_err().contains("TypeError"));
}

#[test]
fn invalid_operands_returns_defaults_and_initializers_never_create_artifacts() {
    for (source, message) in [
        ("fn main(){42?}", "Optional"),
        (
            "fn get()->?i64{42};fn read()->?i64{get()?{return null}};fn main(){42}",
            "Optional",
        ),
        (
            "fn read()->?i64{let value:?i64=null;(value?{return null})};fn main(){42}",
            "Optional",
        ),
        (
            "fn read()->i64{let value:?i64=null;value?};fn main(){42}",
            "Optional or Any callable result",
        ),
        (
            "fn get()->?i64{42};fn f(.x:i64=get()?)->i64{x};fn main(){42}",
            "default",
        ),
        (
            "fn get()->?i64{42};struct P{x:i64=get()?};fn main(){42}",
            "default",
        ),
        (
            "fn get()->?i64{42};global value:?i64=get()?;fn main(){42}",
            "only allowed inside a function or handler body",
        ),
        (
            "fn get()->?i64{42};get()?;fn main(){42}",
            "only allowed inside a function or handler body",
        ),
    ] {
        rejected_with_message(source, message);
    }
}

#[test]
fn defaults_allow_propagation_only_inside_a_nested_callable() {
    assert_value(
        "fn f(.callback:fn()->?i64=||->?i64{let value:?i64=42;value?})->i64{if callback()==42{42}else{0}};fn main(){f()}",
    );
    assert_value(
        "struct P{callback:fn()->?i64=||->?i64{let value:?i64=42;value?}};fn main(){let p=P{};if p.callback()==42{42}else{0}}",
    );
}

#[test]
fn some_patterns_bind_inner_for_match_matches_and_filtered_for() {
    for source in [
        "fn main(){let value:?i64=42;value match{x?=>x,null=>0}}",
        "fn main(){let value:?i8=42;value match{x? if type_of(x)==i8=>x,null=>0,_=>0}}",
        "fn main(){let value:?i64=null;value match{x?=>0,null=>42}}",
        "fn main(){let value:?i64=42;if (value matches x? if x==42){42}else{0}}",
        "fn main(){var count:i64=0;for x? in [null,40,null,2]{if x!=null{count+=1}};if count==2{42}else{0}}",
        "fn main(){let value:?(i64,i64)=(40,2);value match{(x,y)?=>x+y,_=>0}}",
    ] {
        assert_value(source);
    }
    let resolved = checked("fn answer(value:?i64)->i64{value match{x?=>x,null=>0}};fn main(){42}");
    let x = resolved
        .symbols
        .iter()
        .find(|s| str_interner::get(s.name) == "x")
        .unwrap();
    assert_eq!(
        resolved.type_pool.as_intrinsic(x.type_index),
        Some(Intrinsic::I64)
    );
}

#[test]
fn some_patterns_follow_composite_scope_and_snapshot_rules() {
    for source in [
        "fn main(){let value:?i64=42;value match{(x? as whole) if whole==42=>x,_=>0}}",
        "fn main(){let value:?i64=null;value match{not x?=>42,_=>0}}",
        "fn main(){let value:?i64=42;value match{x? and x+0 is answer=>answer,_=>0}}",
        "fn main(){let value:?i64=42;value match{(x? if false) or (x? if true)=>x,_=>0}}",
        "fn main(){let xs=[40,2];xs match{[a? if if true{xs(1)=null;true}else{false},b?]=>if a==40 and b==2{42}else{0},_=>0}}",
    ] {
        assert_value(source);
    }
    for source in [
        "fn main(){let value:?i64=42;let x?=value;42}",
        "fn f(x?:?i64)->i64{42};fn main(){42}",
        "fn main(){let value:?i64=null;value match{not x?=>x,_=>0}}",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(result.codegen_output.functions.is_empty());
        assert!(result.into_artifact().is_err());
    }
}

#[test]
fn optional_unwrap_preserves_exact_inner_value_type_and_heap_identity() {
    for source in [
        "typealias Small=i8;fn main(){let value:?Small=42;let answer=value.unwrap();if type_of(answer)==i8{answer}else{0}}",
        "fn main(){let value:?(i64,i64)=(40,2);let pair=value.unwrap();pair.0+pair.1}",
        "struct Cell{n:i64};fn main(){let cell=Cell{n:40};let value:?Cell=cell;let unwrapped=value.unwrap();unwrapped.n=unwrapped.n+2;cell.n}",
        "fn main(){let value:?fn()->i64=||42;value.unwrap()()}",
        "struct P{};impl P{pub fn unwrap(self)->i64{42}};fn main(){P{}.unwrap()}",
        "struct P{};impl P{pub fn unwrap(self)->i64{42}};fn main(){let value:Any=P{};value.unwrap()}",
        "fn main(){let value:?Unit=();if value.unwrap()==(){42}else{0}}",
    ] {
        assert_value(source);
    }
    let error = common::run_value("fn main(){let value:?i64=null;value.unwrap();42}").unwrap_err();
    assert!(
        error.contains("panic") || error.contains("Panic"),
        "{error}"
    );
}

#[test]
fn scoped_associated_defaults_specialize_optional_inner_and_callbacks() {
    for source in [
        "struct P{text:String};trait Source{assoc Item:Type=Self;fn next(self)->?Item;derive fn callback(self)->?fn()->Item{let item=self.next()?;||->Item{item}}};impl Source for P{pub fn next(self)->?P{self}};fn main(){let callback=P{text:\"ab\"++\"cdefgh\"}.callback().unwrap();if type_of(callback)==(fn()->P){callback().text.len()+34}else{0}}",
        "struct P{};trait Read{fn get(self)->?i64;derive fn answer(self)->?i64{let value=self.get()?;value}};mod a{extend Read for P{pub fn get(self)->?i64{40}};pub fn answer()->?i64{P{}.answer()}};mod b{extend Read for P{pub fn get(self)->?i64{2}};pub fn answer()->?i64{P{}.answer()}};fn main(){a.answer().unwrap()+b.answer().unwrap()}",
    ] {
        assert_value(source);
    }
}

#[test]
fn null_only_optional_propagation_has_noreturn_payload_and_real_null_exit() {
    let source =
        "fn get()->?NoReturn{null};fn read(){get()?};fn main(){if read()==null{42}else{0}}";
    let resolved = checked(source);
    let ret = resolved
        .type_pool
        .canonical_type(result_type(&resolved, "read"))
        .unwrap();
    let TypeKind::Optional { inner } = resolved.type_pool.get(ret).kind else {
        panic!("null-only return must stay Optional")
    };
    assert_eq!(
        resolved.type_pool.as_intrinsic(inner),
        Some(Intrinsic::NoReturn)
    );
    let mut count = 0;
    for (index, node) in resolved.ast.nodes.iter().enumerate() {
        if node.kind == NodeKind::OptionPropagation {
            assert_eq!(
                resolved
                    .type_pool
                    .as_intrinsic(resolved.node_types[&NodeIndex(index as u32)]),
                Some(Intrinsic::NoReturn)
            );
            count += 1;
        }
    }
    assert_eq!(count, 1);
    assert_value(source);
    let source = "fn read(){if true{42}else{null.unwrap()}};fn main(){read()}";
    let resolved = checked(source);
    assert_eq!(
        resolved
            .type_pool
            .as_intrinsic(result_type(&resolved, "read")),
        Some(Intrinsic::I64)
    );
    assert_value(source);
}

#[test]
fn optional_unwrap_rejects_arguments_and_bound_values_without_artifacts() {
    rejected_with_message(
        "fn main(){let value:?i64=42;value.unwrap(0)}",
        "expects 0 arguments",
    );
    rejected_with_message(
        "fn main(){let value:?i64=42;let f=value.unwrap;42}",
        "bound",
    );
}

#[test]
fn optional_boolean_propagation_conditions_keep_ordinary_then_and_else_blocks() {
    for source in [
        "fn read()->?i64{let value:?bool=true;if value?{42}else{0}};fn main(){if read()==42{42}else{0}}",
        "fn read()->?i64{let value:?bool=false;if value?{0}else{42}};fn main(){if read()==42{42}else{0}}",
        "fn read()->?i64{let value:?bool=null;if value?{0}else{0}};fn main(){if read()==null{42}else{0}}",
    ] {
        assert_value(source);
    }
}
