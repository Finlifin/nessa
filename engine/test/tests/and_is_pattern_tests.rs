mod common;

fn assert_value(source: &str, expected: i64) {
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        expected,
        "{source}"
    );
}

#[test]
fn documented_tuple_constraint_and_computed_patterns_export_successful_bindings() {
    for source in [
        "fn main(){(1,42) match {(a,b) and a is 1=>b,_=>0}}",
        "fn main(){(0,42) match {(a,b) and a is 1=>0,_=>42}}",
        "fn main(){40 match {x and x+2 is y=>y,_=>0}}",
        "fn double(x:i64)->i64{x*2};fn main(){21 match {x and double(x) is y=>y,_=>0}}",
        "fn main(){40 match {x and (|n:i64|n+2)(x) is answer=>answer,_=>0}}",
        "struct P{n:i64};fn main(){P{n:42} match {x and x.n is answer=>answer,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn rhs_patterns_use_the_computed_type_instead_of_the_original_input_type() {
    for source in [
        "fn main(){40 match {x and \"abcd\"++\"efgh\" is text=>if text'type==String{text.len()+34}else{0},_=>0}}",
        "enum E{some(n:i64),none};fn main(){40 match {x and E.some(x+2) is E.some(answer)=>answer,_=>0}}",
        "fn main(){40 match {x and (x,2) is (a,b)=>a+b,_=>0}}",
        "fn main(){let value:Any=40;value match {x and x.as(i64)+2 is answer=>if answer'type==i64{answer}else{0},_=>0}}",
        "fn main(){42 match {x and null is null=>x,_=>0}}",
        "fn main(){42 match {x and () is ()=>x,_=>0}}",
        "fn main(){42 match {x and '界' is not '🦀'=>x,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn constraints_are_left_associative_and_aliases_bind_the_correct_input() {
    for source in [
        "fn main(){40 match {x and x+1 is y and y+1 is z=>z,_=>0}}",
        "fn main(){40 match {x and x+2 is y as computed=>if computed==42 and y==42{computed}else{0},_=>0}}",
        "fn main(){40 match {(x and x+2 is y) as whole=>if whole==40 and whole'type==i64{y}else{0},_=>0}}",
        "fn main(){42 match {0 as x or x and x is 42=>x,_=>0}}",
        "fn main(){42 match {x and x is y if x==42 and y==42=>y,_=>0}}",
        "fn main(){42 match {x and x is (y if y==42)=>y,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn alternatives_merge_both_constraint_sides_and_replace_failed_path_values() {
    for source in [
        "enum E{a(n:i64),b(n:i64)};fn main(){E.b(40) match {(E.a(x) and x+2 is y) or (E.b(x) and x+2 is y)=>y,_=>0}}",
        "fn main(){42 match {(x and 0 is y if false) or (x and x is y)=>y,_=>0}}",
        "typealias Small=i64;enum E{a(n:Small),b(n:i64)};fn main(){E.b(40) match {(E.a(x) and x+2 is y) or (E.b(x) and x+2 is y)=>y,_=>0}}",
        "fn main(){42 match {x and x is (0 or 42)=>x,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn successful_lhs_runs_rhs_once_and_failed_lhs_skips_all_rhs_effects() {
    for (source, expected) in [
        (
            "global trace:i64=0;fn input()->i64{trace=trace*10+1;40};fn step(x:i64)->i64{trace=trace*10+2;x+2};fn mark()->bool{trace=trace*10+3;true};fn main(){let result=input() match {x and step(x) is y if mark()=>y,_=>0};trace*100+result}",
            12342,
        ),
        (
            "global calls:i64=0;fn step()->i64{calls+=1;42};fn main(){let result=1 match {0 and step() is x=>0,_=>42};result+calls}",
            42,
        ),
        (
            "global trace:i64=0;fn step(n:i64)->i64{trace=trace*10+n;n};fn main(){let result=42 match {(x and step(1) is 0) or (x and step(2) is 2)=>x,_=>0};trace*100+result}",
            1242,
        ),
        (
            "global trace:i64=0;fn step(n:i64)->i64{trace=trace*10+n;n};fn main(){let result=42 match {(x and step(1) is 1) or (x and step(2) is 2)=>x,_=>0};trace*100+result}",
            142,
        ),
        (
            "global trace:i64=0;fn step(n:i64)->i64{trace=trace*10+n;n};fn main(){let result=42 match {x and step(1) is 0 and step(2) is 2=>0,_=>42};trace*100+result}",
            142,
        ),
    ] {
        assert_value(source, expected);
    }
}

#[test]
fn rhs_values_are_snapshotted_before_pattern_guards_mutate_their_source() {
    for source in [
        "fn main(){var n:i64=42;1 match {x and n is (y if if true{n=0;true}else{false})=>if n==0{y}else{0},_=>0}}",
        "fn main(){var n:i64=42;1 match {x and n is ((42 as y if if true{n=0;false}else{true}) or y)=>y,_=>0}}",
        "fn main(){let y=40;2 match {x and y+2 is y=>if x==2{y}else{0},_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn negated_constraints_keep_bindings_private_and_reverse_the_complete_result() {
    for source in [
        "fn main(){40 match {not (x and x+2 is 0)=>42,_=>0}}",
        "fn main(){40 match {not (x and x+2 is 42)=>0,_=>42}}",
        "fn main(){let y=40;1 match {not (x and x is y if y==0)=>y+2,_=>0}}",
        "fn main(){42 match {x and x is not (y if y==0)=>x,_=>0}}",
        "fn main(){40 match {not not (x and x+2 is 42)=>42,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn matches_guards_and_for_filters_see_successful_constraint_bindings() {
    for source in [
        "fn main(){if (40 matches x and x+2 is y if x==40 and y==42){42}else{0}}",
        "fn main(){var total:i64=0;for x and x is not 0 as value in [0,40,0,2]{total+=value.as(i64)};total}",
        "fn main(){let values=Map();values(\"skip\")=100;values(\"take-a\")=40;values(\"take-b\")=2;var total:i64=0;for (key,value) and key is not \"skip\" in values{total+=value.as(i64)};total}",
        "fn main(){var total:i64=0;for x and x.as(i64)+1 is y if y>1 in [0,39,1]{total+=y};total}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn closures_capture_both_sides_after_the_successful_constraint() {
    for source in [
        "fn make()->fn()->i64{40 match {x and x+2 is y=>||y,_=>||0}};fn main(){let f=make();f()}",
        "fn make()->fn()->i64{40 match {x and x+2 is y=>||x+y-40,_=>||0}};fn main(){let f=make();f()}",
        "fn main(){let values=[40];let f=values match {x and x(0).as(i64)+2 is y=>||{x.push(y);x(1).as(i64)},_=>||0};let answer=f();if values.len()==2{answer}else{0}}",
        "fn main(){let callbacks=List();for x and x.as(i64)+1 is y if y>1 in [0,39,1]{callbacks.push(||x.as(i64)+y)};callbacks(0).as(fn()->i64)()+callbacks(1).as(fn()->i64)()-40}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn invalid_constraint_syntax_binding_types_and_contexts_reject_artifacts() {
    for (source, expected_diagnostic) in [
        ("fn main(){42 match {x and is 42=>42,_=>0}}", "expression"),
        ("fn main(){42 match {x and x 42=>42,_=>0}}", "is"),
        ("fn main(){42 match {x and x is=>42,_=>0}}", "pattern"),
        (
            "fn main(){42 match {x and future is future=>42,_=>0}}",
            "undefined",
        ),
        (
            "fn main(){42 match {x and x is (y if future==42) and y is future=>42,_=>0}}",
            "undefined",
        ),
        ("fn main(){42 match {x and x is x=>42,_=>0}}", "same name"),
        (
            "fn main(){42 match {(x and x is y) or x=>42,_=>0}}",
            "same names",
        ),
        (
            "fn main(){42 match {(x and x is y) or (x and \"text\" is y)=>42,_=>0}}",
            "same type",
        ),
        (
            "fn main(){42 match {x and \"text\" is 42=>42,_=>0}}",
            "pattern literal",
        ),
        (
            "fn main(){42 match {x and x is y if 42=>42,_=>0}}",
            "pattern guard",
        ),
        ("fn main(){42 matches x and x is y;y}", "undefined"),
        (
            "fn main(){42 match {not (x and x is y)=>y,_=>0}}",
            "undefined",
        ),
        ("fn main(){let x and x is 42=42;42}", "supported"),
        (
            "fn read(x and x is 42:i64)->i64{42};fn main(){read(42)}",
            "supported",
        ),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(expected_diagnostic)),
            "{source}: {:?}",
            compiled.diagnostics
        );
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
fn failed_constraint_match_reports_no_matching_case() {
    let error = common::run_value("fn main(){42 match {x and x is 0=>42}}").unwrap_err();
    assert!(error.contains("NoMatchingCase"), "{error}");
}
