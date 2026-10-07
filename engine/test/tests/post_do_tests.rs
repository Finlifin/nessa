mod common;

fn assert_value(source: &str, expected: i64) {
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        expected,
        "{source}"
    );
}

#[test]
fn list_methods_accept_bare_and_existing_argument_trailing_callbacks() {
    for source in [
        "fn main(){let xs=[20,21].map do |x|x+1;xs.fold(0,|acc,x|acc+x).as(i64)-1}",
        "fn main(){let xs=[20,21].map() do |x|x+1;xs.fold(0,|acc,x|acc+x).as(i64)-1}",
        "fn main(){let xs=[0,40,2].filter do |x|x.as(i64)>0;xs.fold(0) do |acc,x|acc+x}",
        "global total:i64=0;fn main(){[40,2].each do |x|{total+=x.as(i64)};total}",
        "global total:i64=0;fn main(){[40,2].foreach do |x|{total+=x.as(i64)};total}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn block_and_explicit_zero_and_multiple_parameter_callbacks_use_ordinary_calls() {
    for source in [
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){invoke do {42}}",
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){invoke do ||42}",
        "fn invoke(base:i64,f:fn(i64,i64)->i64)->i64{f(base,2)};fn main(){invoke(40) do |a,b|a+b}",
        "fn invoke(f:fn(i64,i64,i64)->i64)->i64{f(30,10,2)};fn main(){invoke do |a:i64,b:i64,c:i64|a+b+c}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn callbacks_infer_parameters_and_returns_from_function_alias_context() {
    for source in [
        "typealias Step=fn(i64)->i64;fn invoke(f:Step)->i64{if type_of(f)==Step{f(40)}else{0}};fn main(){invoke do |x|x+2}",
        "typealias Pred=fn(i64)->bool;fn invoke(f:Pred)->i64{if type_of(f)==Pred and f(42){42}else{0}};fn main(){invoke do |x|x>40}",
        "fn invoke(f:fn(i64)->i64)->i64{f(40)};fn main(){invoke do |x:i64|->i64{x+2}}",
        "fn invoke(f:fn()->i64)->i64{if type_of(f)==(fn()->i64){f()}else{0}};fn main(){invoke do {42}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn appended_callback_keeps_named_default_and_variadic_binding_rules() {
    for source in [
        "fn invoke(base:i64,f:fn(i64)->i64,.extra:i64=0)->i64{base+f(1)+extra};fn main(){invoke(40,extra=0) do |x|x+1}",
        "fn invoke(base:i64,f:fn()->i64,.extra:i64=2)->i64{base+f()+extra};fn main(){invoke(40) do {0}}",
        "global defaults:i64=0;fn fallback()->i64{defaults+=1;2};fn invoke(f:fn()->i64,.extra:i64=fallback())->i64{f()+extra};fn main(){let answer=invoke(extra=2) do {40};answer+defaults}",
        "fn invoke(base:i64,...args:List)->i64{base+args(0).as(i64)+args(1).as(fn()->i64)()};fn main(){invoke(40,1) do ||->i64{1}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn function_values_any_values_returned_functions_and_custom_apply_accept_do() {
    for source in [
        "fn invoke(f:fn(i64)->i64)->i64{f(40)};fn main(){let target:fn(fn(i64)->i64)->i64=invoke;target do |x|x+2}",
        "fn invoke(f:fn(i64)->i64)->i64{f(40)};fn main(){let target:Any=invoke;target do |x:i64|->i64{x+2}}",
        "fn factory()->fn(fn(i64)->i64)->i64{|f:fn(i64)->i64|f(40)};fn main(){factory()() do |x|x+2}",
        "struct Invoke{base:i64};impl Invoke{pub fn apply(self,f:fn(i64)->i64)->i64{f(self.base)}};fn main(){let target=Invoke{base:40};target do |x|x+2}",
        "struct Invoke{base:i64};impl Invoke{pub fn apply(self,extra:i64,f:fn(i64)->i64)->i64{f(self.base)+extra}};fn main(){Invoke{base:39}(1) do |x|x+2}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn callee_existing_arguments_and_callback_body_run_once_in_source_order() {
    let source = r#"
        global trace:i64=0;
        fn step(n:i64)->i64{trace=trace*10+n;n};
        fn factory()->fn(i64,i64,fn(i64)->i64)->i64{
            step(1);
            |a:i64,b:i64,f:fn(i64)->i64|{let value=f(a+b);if trace==1234{value}else{0}}
        };
        fn main(){factory()(step(2),step(3)) do |x|{step(4);x+37}}
    "#;
    assert_value(source, 42);
    assert_value(
        "global trace:i64=0;fn step(n:i64)->i64{trace=trace*10+n;n};fn invoke(a:i64,f:fn()->i64,.b:i64=0)->i64{let n=f();if trace==214{a*10+b+n}else{0}};fn main(){invoke(b=step(2),step(1)) do {step(4);30}}",
        42,
    );
    assert_value(
        "fn invoke(x:i64,f:fn()->i64)->i64{f()};fn main(){var n:i64=1;invoke(if true{n=40;n}else{0}) do {n+2}}",
        42,
    );
}

#[test]
fn returned_callbacks_capture_values_and_share_heap_references() {
    let source = r#"
        fn preserve(f:fn()->i64)->fn()->i64{f};
        fn make(values:List)->fn()->i64{
            let extra=2;
            preserve do {values.push(extra);values(0).as(i64)+extra}
        };
        fn main(){
            let values=[40];
            let f=make(values);
            let first=f();
            values(0)=41;
            let second=f();
            if first==42 and second==43 and values.len()==3{42}else{0}
        }
    "#;
    assert_value(source, 42);
}

#[test]
fn null_unit_and_nested_parenthesized_chains_keep_callback_results() {
    for source in [
        "fn main(){let xs=[null,(),42].map do |x|x;let nil=[1].map do |x|null;let unit=[1].map do |x|();if xs(0)==null and xs(1)==() and nil(0)==null and unit(0)==(){xs(2).as(i64)}else{0}}",
        "fn main(){let xs=([1,2,3].map do |x|x*10).filter do |x|x.as(i64)>10;(xs.fold(0) do |acc,x|acc+x).as(i64)-8}",
        "fn main(){let xs=[1,2].map do |x|([10,20].fold(0) do |acc,y|acc+x*y);xs.fold(0,|acc,x|acc+x).as(i64)-48}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn return_in_do_block_returns_from_callback_and_preserves_outer_execution() {
    for source in [
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){let answer=invoke do {return 40};answer+2}",
        "fn invoke(f:fn()->i64)->i64{f()+f()};fn main(){let answer=invoke do {if true{return 20}else{0}};answer+2}",
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){let answer=invoke do {while true{break};return 40};answer+2}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn invalid_static_do_shapes_results_callees_and_control_produce_no_artifact() {
    for source in [
        "fn main(){[1].map do ||42}",
        "fn main(){[1].fold(0) do |x|x}",
        "fn main(){[1].filter do |x|42}",
        "fn main(){[1].each do |x|true}",
        "fn main(){[1].foreach do {()}}",
        "fn main(){42 do |x|x}",
        "struct P{};fn main(){let p=P{};p do {42}}",
        "fn invoke(a:i64,f:fn()->i64)->i64{a+f()};fn main(){invoke do {42}}",
        "fn invoke(.f:fn()->i64=||42)->i64{f()};fn main(){invoke do {42}}",
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){invoke do {break;42}}",
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){invoke do {continue;42}}",
        "fn invoke(f:fn(i64)->i64)->i64{f(40)};fn main(){invoke do |x:String|x.len()}",
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){invoke(f=||42) do {42}}",
        "fn invoke(a:i64,b:i64,f:fn()->i64)->i64{a+b+f()};fn main(){invoke(b=2,a=1) do {39}}",
        "fn invoke(a:i64,b:i64,f:fn()->i64)->i64{a+b+f()};fn main(){invoke(b=2,a=1,||39)}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled.codegen_output.functions.is_empty(),
            "produced code for {source}"
        );
        assert!(
            compiled.into_artifact().is_err(),
            "produced artifact for {source}"
        );
    }
}

#[test]
fn any_callees_and_callback_values_still_check_runtime_contracts() {
    for (source, expected_error) in [
        (
            "fn main(){let target:Any=42;target do |x|x}",
            "MethodNotFound",
        ),
        (
            "fn invoke(f:fn()->i64)->i64{f()};fn main(){let target:Any=invoke;target do |x:i64|42}",
            "TypeError",
        ),
        (
            "fn invoke(f:fn(i64)->bool)->bool{f(42)};fn main(){let target:Any=invoke;target do |x:i64|->i64{x}}",
            "TypeError",
        ),
        (
            "fn main(){let target:Any=List();target do |x|x}",
            "TypeError",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(expected_error), "{source}: {error}");
    }
}
