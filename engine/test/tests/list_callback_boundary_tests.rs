mod common;

#[test]
fn list_callback_methods_execute_dynamic_contracts_and_snapshot_order() {
    for source in [
        "fn main(){let xs=[1,2,3].map(|x|x*2);xs(0).as(i64)+xs(1).as(i64)+xs(2).as(i64)+30}",
        "fn main(){let xs=[40,0,2].filter(|x|x.as(i64)>0);xs.fold(0,|acc,x|acc+x)}",
        "global total:i64=0;fn main(){[40,2].each(|x|{total+=x.as(i64)});total}",
        "global total:i64=0;fn main(){[40,2].foreach(|x|{total+=x.as(i64)});total}",
        "fn main(){let xs=[40,2];let mapped=xs.map(|x|{xs(0)=0;xs.push(100);x});mapped.fold(0,|acc,x|acc+x)}",
        "fn main(){let xs=[null,(),42].map(|x|x);if xs.len()==3 and xs(0)==null and xs(1)==(){xs(2).as(i64)}else{0}}",
        "fn main(){let xs:List=List();xs.fold(42,|acc,x|0)}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn invalid_callback_shapes_and_static_results_do_not_produce_artifacts() {
    for source in [
        "fn main(){[42].map(||42)}",
        "fn main(){[42].map(|a,b|42)}",
        "fn main(){[42].filter(|x|42)}",
        "fn main(){[42].each(|x|42)}",
        "fn main(){[42].fold(0,|x|42)}",
        "fn main(){[42].foreach(42)}",
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
fn typed_and_erased_callback_boundaries_check_values_and_results() {
    for source in [
        "fn constant(x:i64)->i64{42};fn main(){[0].map(constant)(0).as(i64)}",
        "fn constant(x:i64)->i64{42};fn main(){let f:Any=constant;[0].map(f)(0).as(i64)}",
        "fn main(){let f:Any=|x:i64|42;[0].map(f)(0).as(i64)}",
        "fn main(){let f:fn(Any)->Any=|x|();[null,()].each(f);42}",
        "fn equal(x:f64)->bool{x==42.0};fn main(){if [42].filter(equal).len()==1{42}else{0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
    for source in [
        "fn constant(x:i64)->i64{42};fn main(){[true].map(constant)}",
        "fn constant(x:i64)->i64{42};fn main(){let f:Any=constant;[true].map(f)}",
        "fn main(){let f:fn(Any)->Any=|x|42;[0].filter(f)}",
        "fn main(){let f:fn(Any)->Any=|x|null;[0].filter(f)}",
        "fn main(){let f:fn(Any)->Any=|x|42;[0].each(f)}",
        "fn main(){let f:fn(Any)->Any=|x|null;[0].foreach(f)}",
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}

#[test]
fn callback_initializers_load_the_globals_that_the_callback_actually_reads() {
    for source in [
        "mod api{pub global answer:List=[0].map(|x|storage.n)};mod storage{pub global n:i64=42;pub fn unused()->List{api.answer}};fn main(){api.answer(0).as(i64)}",
        "mod api{pub global answer:List=[0].map(|x|storage.n)};mod storage{pub global n:i64=42};fn main(){api.answer(0).as(i64)}",
        "fn apply(f:fn(Any)->Any)->Any{let copy=f;copy(0)};mod api{pub global answer:Any=apply(|x|storage.n)};mod storage{pub global n:i64=42;pub fn unused()->Any{api.answer}};fn main(){api.answer.as(i64)}",
        "fn apply(f:fn(Any)->Any)->List{let copy=f;[0].map(copy)};mod api{pub global answer:List=apply(|x|storage.n)};mod storage{pub global n:i64=42;pub fn unused()->List{api.answer}};fn main(){api.answer(0).as(i64)}",
        "fn apply()->List{let f=|x:Any|storage.n;[0].map(f)};mod api{pub global answer:List=apply()};mod storage{pub global n:i64=42;pub fn unused()->List{api.answer}};fn main(){api.answer(0).as(i64)}",
        "fn apply(f:fn(Any)->Any)->List{[0].map(|x|f(x))};mod api{pub global answer:List=apply(|x|storage.n)};mod storage{pub global n:i64=42;pub fn unused()->List{api.answer}};fn main(){api.answer(0).as(i64)}",
        "mod api{fn get(x:Any)->Any{storage.n};pub global answer:List=[0].map(get)};mod storage{pub global n:i64=42};fn main(){api.answer(0).as(i64)}",
        "mod api{pub global callback:fn(Any)->Any=|x|storage.n;pub global answer:List=[0].map(callback)};mod storage{pub global n:i64=42};fn main(){api.answer(0).as(i64)}",
        "fn apply(f:fn(Any)->Any)->List{[0].map(f)};mod api{pub global answer:List=apply(|x|storage.n)};mod storage{pub global n:i64=42};fn main(){api.answer(0).as(i64)}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn callback_construction_and_statically_unreached_calls_do_not_add_execution_edges() {
    for source in [
        "fn ignore(f:fn(Any)->Any)->i64{42};mod api{pub global answer:i64=ignore(|x|storage.n)};mod storage{pub global n:i64=api.answer};fn main(){storage.n}",
        "fn ignore(f:fn(Any)->Any)->i64{if false{f(0)};42};mod api{pub global answer:i64=ignore(|x|storage.n)};mod storage{pub global n:i64=api.answer};fn main(){storage.n}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}
