mod common;

fn assert_value(source: &str, expected: i64) {
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        expected,
        "{source}"
    );
}

#[test]
fn negation_complements_scalar_null_unit_and_unicode_predicates() {
    for source in [
        "fn main(){42 match {not 0=>42,_=>0}}",
        "fn main(){0 match {not 0=>0,_=>42}}",
        "fn main(){false match {not true=>42,_=>0}}",
        "fn main(){\"right\" match {not \"left\"=>42,_=>0}}",
        "fn main(){'界' match {not '🦀'=>42,_=>0}}",
        "fn main(){'🦀' match {not '🦀'=>0,_=>42}}",
        "fn main(){let value:Any=null;value match {not null=>0,_=>42}}",
        "fn main(){let value:Any=();value match {not null=>42,_=>0}}",
        "fn main(){let value:Any=null;value match {not ()=>42,_=>0}}",
        "fn main(){let value:Any=();value match {not ()=>0,_=>42}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn negation_checks_complete_enum_and_tuple_children_before_inverting() {
    for source in [
        "enum E{a,b};fn main(){E.b match {not E.a=>42,_=>0}}",
        "enum E{a,b};fn main(){E.a match {not E.a=>0,_=>42}}",
        "enum E{pair(a:i64,b:i64),none};fn main(){E.pair(40,2) match {not E.pair(40,0)=>42,_=>0}}",
        "enum E{pair(a:i64,b:i64),none};fn main(){E.pair(40,2) match {not E.pair(40,2)=>0,_=>42}}",
        "enum E{pair(a:i64,b:i64),none};fn main(){E.none match {not E.pair(_,_)=>42,_=>0}}",
        "fn main(){(40,2) match {not (40,0)=>42,_=>0}}",
        "fn main(){(40,2) match {not (40,2)=>0,_=>42}}",
        "enum E{pair(a:i64,b:i64)};fn main(){E.pair(1,42) match {E.pair(not 0,n)=>n,_=>0}}",
        "fn main(){(1,42) match {(not 0,n)=>n,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn dynamic_negation_preserves_nominal_and_scalar_kind_checks() {
    for source in [
        "enum A{one};enum B{one};fn main(){let value:Any=B.one;value match {not A.one=>42,_=>0}}",
        "enum E{some(n:i64),none};fn main(){let value:Any=42;value match {not E.some(_)=>42,_=>0}}",
        "fn main(){let value:Any=\"界\";value match {not '界'=>42,_=>0}}",
        "fn main(){let value:Any=30028;value match {not '界'=>42,_=>0}}",
        "fn main(){let value:Any='界';value match {not '界'=>0,_=>42}}",
        "fn main(){let value:Any=null;value match {not 42=>42,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn nested_negation_and_alias_precedence_keep_outer_bindings_precise() {
    for source in [
        "fn main(){0 match {not not 0=>42,_=>0}}",
        "fn main(){42 match {not not not 0=>42,_=>0}}",
        "fn main(){42 match {not (0 or 1) as whole=>whole,_=>0}}",
        "fn main(){42 match {not 0 as whole if whole==42=>whole,_=>0}}",
        "fn main(){let value:i8=42;value match {not 0 as whole=>if whole'type==i8{whole}else{0},_=>0}}",
        "fn main(){2 match {not 0 or 1=>42,_=>0}}",
        "fn main(){0 match {not 0 or 0=>42,_=>0}}",
        "fn main(){0 match {not _=>0,_=>42}}",
        "fn main(){42 match {not name=>0,_=>42}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn child_guards_use_private_bindings_without_shadowing_outer_names() {
    for source in [
        "fn main(){let n=40;1 match {not (n if n==0)=>n+2,_=>0}}",
        "fn main(){let whole=40;1 match {not (n as whole if whole==0)=>whole+2,_=>0}}",
        "fn main(){let n=40;1 match {not (n if n==0) as value if value==1 and n==40=>n+2,_=>0}}",
        "fn main(){let n=40;if (1 matches not (n if n==0)){n+2}else{0}}",
        "fn main(){let n=40;(1,42) match {(not (n if n==0),answer)=>if n==40{answer}else{0},_=>0}}",
        "fn main(){let n=40;1 match {not not (n if n==1)=>n+2,_=>0}}",
        "typealias Small=i64;enum E{a(n:Small),b(n:i64)};fn main(){E.b(1) match {not ((E.a(n) or E.b(n)) if n==0)=>42,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn complete_child_guards_and_alternatives_keep_short_circuit_side_effect_order() {
    for (source, expected) in [
        (
            "global trace:i64=0;fn mark(n:i64,b:bool)->bool{trace=trace*10+n;b};fn main(){let result=1 match {not ((x if mark(1,false)) or (x if mark(2,true)))=>0,_=>42};result+trace*100}",
            1242,
        ),
        (
            "global trace:i64=0;fn mark(n:i64,b:bool)->bool{trace=trace*10+n;b};fn main(){let result=1 match {not ((x if mark(1,true)) or (x if mark(2,true)))=>0,_=>42};result+trace*100}",
            142,
        ),
        (
            "global trace:i64=0;fn mark(n:i64,b:bool)->bool{trace=trace*10+n;b};fn main(){let result=1 match {not ((x if mark(1,false)) or (x if mark(2,false))) if mark(3,true)=>42,_=>0};result+trace*100}",
            12342,
        ),
        (
            "global trace:i64=0;fn mark(n:i64,b:bool)->bool{trace=trace*10+n;b};fn main(){let result=1 match {not (0 if mark(1,true))=>42,_=>0};result+trace*100}",
            42,
        ),
        (
            "global trace:i64=0;fn mark(n:i64,b:bool)->bool{trace=trace*10+n;b};fn main(){let result=1 match {not (x if mark(1,false)) if mark(2,false)=>0,_=>42};result+trace*100}",
            1242,
        ),
    ] {
        assert_value(source, expected);
    }
}

#[test]
fn negated_matching_evaluates_scrutinees_once_and_keeps_input_snapshot() {
    for source in [
        "global calls:i64=0;fn next()->i64{calls+=1;42};fn main(){let result=next() match {not 0 as whole=>whole,_=>0};if calls==1{result}else{0}}",
        "global calls:i64=0;fn next()->i64{calls+=1;42};fn main(){let result=next() matches not (0 or 1);if calls==1 and result{42}else{0}}",
        "fn main(){var input:i64=42;let result=input match {not (0 if if true{input=0;true}else{false}) as whole=>whole,_=>0};result}",
        "fn main(){var input:i64=42;let result=input match {not (x if if true{input=0;false}else{true}) as whole=>whole,_=>0};if input==0{result}else{0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn negated_matches_return_booleans_and_for_loops_skip_failed_inputs() {
    for source in [
        "fn main(){if (42 matches not 0) and not (0 matches not 0){42}else{0}}",
        "fn main(){if (42 matches not (x if x==0)) and not (0 matches not (x if x==0)){42}else{0}}",
        "fn main(){var sum:i64=0;for not 0 as value in [0,40,0,2]{sum+=value.as(i64)};sum}",
        "fn main(){var sum:i64=0;for not (x if x==0) as value in [0,40,0,2]{sum+=value.as(i64)};sum}",
        "fn main(){let values=Map();values(\"\")=100;values(\"a\")=40;values(\"b\")=2;var sum:i64=0;for (not \"\",n) in values{sum+=n.as(i64)};sum}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn negated_bare_enum_variant_patterns_filter_for_loop_inputs() {
    assert_value(
        "enum E{some(n:i64),none};fn main(){var count:i64=0;for not E.none in [E.none,E.some(1),E.none,E.some(2)]{count+=1};count+40}",
        42,
    );
}

#[test]
fn invalid_negation_types_guards_contexts_and_escaped_names_reject_artifacts() {
    for (source, expected_diagnostic) in [
        ("fn main(){42 match {not=>42,_=>0}}", "pattern"),
        ("fn main(){42 matches not}", "pattern"),
        ("fn main(){42 match {not true=>42,_=>0}}", "pattern literal"),
        (
            "fn main(){'界' match {not \"界\"=>42,_=>0}}",
            "pattern literal",
        ),
        (
            "fn main(){42 match {not (x if 42)=>42,_=>0}}",
            "pattern guard",
        ),
        (
            "fn main(){42 match {not (x if false)=>x,_=>0}}",
            "undefined",
        ),
        (
            "fn main(){42 match {not (x if false) if x==42=>42,_=>0}}",
            "undefined",
        ),
        ("fn main(){42 matches not (x if false);x}", "undefined"),
        ("fn main(){for not (x if false) in [42]{x};42}", "undefined"),
        (
            "fn main(){(1,2) match {not ((x if y==2),y)=>42,_=>0}}",
            "undefined",
        ),
        (
            "enum E{a(n:i8),b(n:i64)};fn main(){E.a(1) match {not (E.a(n) or E.b(n))=>42,_=>0}}",
            "same type",
        ),
        (
            "enum E{a(n:i64),b(n:i64)};fn main(){E.a(1) match {not (E.a(x) or E.b(y))=>42,_=>0}}",
            "same names",
        ),
        (
            "enum A{one};enum B{one};fn main(){A.one match {not B.one=>42,_=>0}}",
            "different enum",
        ),
        ("fn main(){let not 0=42;42}", "supported"),
        (
            "fn read(not 0:i64)->i64{42};fn main(){read(42)}",
            "supported",
        ),
        (
            "fn main(){42 match {!0=>42,_=>0}}",
            "Error branch pattern requires an Error-qualified input",
        ),
        (
            "fn main(){let value:Any=(1,2);value match {not (1,2)=>42,_=>0}}",
            "statically known tuple",
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
fn failing_negated_match_reports_no_matching_case() {
    let error = common::run_value("fn main(){42 match {not 42=>0}}").unwrap_err();
    assert!(error.contains("NoMatchingCase"), "{error}");
}
