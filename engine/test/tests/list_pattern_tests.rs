mod common;

fn assert_value(source: &str, expected: i64) {
    assert_eq!(
        common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
        expected,
        "{source}"
    );
}

#[test]
fn exact_list_patterns_check_empty_singleton_and_full_length() {
    for source in [
        "fn main(){[] match {[]=>42,_=>0}}",
        "fn main(){[42] match {[x]=>x.as(i64),_=>0}}",
        "fn main(){[40,2] match {[a,b]=>a.as(i64)+b.as(i64),_=>0}}",
        "fn main(){[40,2] match {[]=>0,[x]=>0,[a,b,c]=>0,[a,b]=>a.as(i64)+b.as(i64),_=>0}}",
        "fn main(){[] match {[x]=>0,_=>42}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn one_rest_in_any_position_binds_a_fresh_list_including_empty_remainders() {
    for source in [
        "fn main(){[1,40,2] match {[first,...rest]=>if first==1 and rest'type==List{rest(0).as(i64)+rest(1).as(i64)}else{0},_=>0}}",
        "fn main(){[40,2,1] match {[...rest,last]=>if last==1{rest(0).as(i64)+rest(1).as(i64)}else{0},_=>0}}",
        "fn main(){[1,40,2,9] match {[first,...rest,last]=>if first==1 and last==9{rest(0).as(i64)+rest(1).as(i64)}else{0},_=>0}}",
        "fn main(){[40,2] match {[a,...rest,b]=>if rest.len()==0{a.as(i64)+b.as(i64)}else{0},_=>0}}",
        "fn main(){[] match {[...rest]=>if rest.len()==0 and rest'type==List{42}else{0},_=>0}}",
        "fn main(){[42] match {[x,...rest]=>if rest.len()==0{x.as(i64)}else{0},_=>0}}",
        "fn main(){[42] match {[...rest,x]=>if rest.len()==0{x.as(i64)}else{0},_=>0}}",
        "fn main(){let xs=[40];xs match {[...rest]=>{rest.push(2);if xs.len()==1{rest(0).as(i64)+rest(1).as(i64)}else{0}},_=>0}}",
        "fn main(){[42] match {[a,...rest,b]=>0,_=>42}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn dynamic_list_patterns_test_nominal_identity_and_nested_values() {
    for source in [
        "fn main(){let value:Any=[40,2];value match {[a,b]=>a.as(i64)+b.as(i64),_=>0}}",
        "fn main(){let value:Any=null;value match {[...rest]=>0,_=>42}}",
        "fn main(){let value:Any=();value match {[]=>0,_=>42}}",
        "fn main(){let value:Any=\"\";value match {[]=>0,_=>42}}",
        "fn main(){let value:Any=Map();value match {[]=>0,_=>42}}",
        "fn main(){let value:Any=42;value match {[]=>0,_=>42}}",
        "struct P{};fn main(){let value:Any=P{};value match {[]=>0,_=>42}}",
        "fn main(){[null,(),'界',42] match {[null,(),'界',answer]=>answer.as(i64),_=>0}}",
        "fn main(){[[40],[2]] match {[[a],[b]]=>a.as(i64)+b.as(i64),_=>0}}",
        "enum E{some(n:i64),none};fn main(){[E.some(42)] match {[E.some(n)]=>n,_=>0}}",
        "enum E{values(items:List)};fn main(){E.values([40,2]) match {E.values([a,b])=>a.as(i64)+b.as(i64),_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn all_input_slots_are_snapshotted_before_element_guards_modify_the_source() {
    for source in [
        "fn main(){let xs=[1,40,2,9];xs match {[a if if true{xs.pop();xs.pop();xs.pop();xs.pop();xs.push(99);xs(0)=100;true}else{false},...rest,z]=>if a==1 and z==9 and xs.len()==1 and xs(0)==100{rest(0).as(i64)+rest(1).as(i64)}else{0},_=>0}}",
        "fn main(){let xs=[40,2];xs match {[a if if true{xs(1)=100;xs.push(100);true}else{false},b]=>a.as(i64)+b.as(i64),_=>0}}",
        "fn main(){let xs=[40,2];xs match {[a if if true{xs.pop();xs.pop();true}else{false},b]=>a.as(i64)+b.as(i64),_=>0}}",
        "fn main(){let xs=[0,0];xs match {[x if if true{xs(0)=40;xs(1)=2;false}else{true},...rest]=>0,[a,b]=>a.as(i64)+b.as(i64),_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn rest_containers_copy_slots_while_sharing_referenced_objects() {
    for source in [
        "struct Cell{n:i64};fn main(){let cell=Cell{n:40};let xs=[cell,cell];xs match {[first if if true{first.as(Cell).n=42;true}else{false},...rest]=>{rest(0).as(Cell).n=43;rest.push(0);if cell.n==43 and xs.len()==2 and rest.len()==2{42}else{0}},_=>0}}",
        "fn main(){let inner=[40];let xs=[inner];xs match {[...rest]=>{rest(0).as(List).push(2);rest(0)=null;if xs(0).as(List).len()==2{inner(0).as(i64)+inner(1).as(i64)}else{0}},_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn list_alternatives_aliases_negation_and_constraints_preserve_binding_contracts() {
    for source in [
        "fn main(){[10,42] match {[x,0] or [_,x]=>x.as(i64),_=>0}}",
        "fn main(){[40,2] match {([0,...rest] or [...rest,0])=>0,[...rest] as whole=>if rest.len()==2 and whole.len()==2{rest(0).as(i64)+whole(1).as(i64)}else{0},_=>0}}",
        "fn main(){[40,2] match {not [0,...rest]=>42,_=>0}}",
        "fn main(){[40,2] match {[a,b] and a.as(i64)+b.as(i64) is answer=>answer,_=>0}}",
        "fn main(){42 match {x and [x] is [answer]=>answer.as(i64),_=>0}}",
        "fn main(){let rest=40;[1] match {not [x if false,...rest]=>rest+2,_=>0}}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn list_scrutinees_and_guards_run_once_in_encounter_order() {
    assert_value(
        "global trace:i64=0;fn input()->List{trace=trace*10+1;[40,2]};fn mark(n:i64)->bool{trace=trace*10+n;true};fn main(){let result=input() match {[a if mark(2),...rest] if mark(3)=>a.as(i64)+rest(0).as(i64),_=>0};trace*100+result}",
        12342,
    );
    assert_value(
        "global calls:i64=0;fn mark()->bool{calls+=1;true};fn main(){let result=[1] match {[a if mark(),b]=>0,_=>42};result+calls}",
        42,
    );
    assert_value(
        "global calls:i64=0;fn mark()->bool{calls+=1;true};fn main(){let result=[1,2] match {[0,x if mark()]=>0,_=>42};result+calls}",
        42,
    );
}

#[test]
fn matches_and_for_filters_support_lists_and_returned_capture_lifetimes() {
    for source in [
        "fn main(){if ([40,2] matches [a,...rest] if a==40 and rest(0)==2){42}else{0}}",
        "fn main(){var sum:i64=0;for [a,b] in [[40,2],[],[100],[1,2,3]]{sum+=a.as(i64)+b.as(i64)};sum}",
        "fn main(){let callbacks=List();for [x,...rest] in [[40,2],[],[1,0]]{callbacks.push(||x.as(i64)+rest.len())};callbacks(0).as(fn()->i64)()+callbacks(1).as(fn()->i64)()-1}",
        "fn make()->fn()->i64{[40,2] match {[x,...rest]=>||x.as(i64)+rest(0).as(i64),_=>||0}};fn main(){make()()}",
    ] {
        assert_value(source, 42);
    }
}

#[test]
fn malformed_rest_types_bindings_and_contexts_reject_code_and_artifacts() {
    for (source, expected) in [
        ("fn main(){42 matches [1]}", "List"),
        ("fn main(){[1] match {[..rest]=>42,_=>0}}", "rest"),
        ("fn main(){[1] match {[...]=>42,_=>0}}", "rest"),
        ("fn main(){[1] match {[..._]=>42,_=>0}}", "rest"),
        ("fn main(){[1] match {[...42]=>42,_=>0}}", "rest"),
        ("fn main(){[1] match {[...a,...b]=>42,_=>0}}", "rest"),
        ("fn main(){[1] match {[...rest as whole]=>42,_=>0}}", "rest"),
        ("fn main(){(1,2) match {(...rest,)=>42,_=>0}}", "rest"),
        ("fn main(){42 match {...rest=>42,_=>0}}", "rest"),
        ("fn main(){[1,2] match {[x,...x]=>42,_=>0}}", "same name"),
        ("fn main(){[1,2] match {[x,x]=>42,_=>0}}", "same name"),
        ("fn main(){[1] match {[x] or [...x]=>42,_=>0}}", "same type"),
        (
            "enum E{some(n:i64)};fn main(){let value:Any=[42];value match {[x] or E.some(x)=>42,_=>0}}",
            "same type",
        ),
        (
            "fn main(){[1] match {[...x] or [y]=>42,_=>0}}",
            "same names",
        ),
        (
            "fn main(){[1] match {[x if rest.len()==0,...rest]=>42,_=>0}}",
            "undefined",
        ),
        ("fn main(){[1] matches [...rest];rest}", "undefined"),
        (
            "fn main(){[1] match {not [...rest]=>rest,_=>[]}}",
            "undefined",
        ),
        ("fn main(){[1] match {[x if 42]=>42,_=>0}}", "pattern guard"),
        ("fn main(){let [x]=[42];x}", "supported"),
        (
            "fn read([x]:List)->i64{42};fn main(){read([42])}",
            "supported",
        ),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled.diagnostics.iter().any(|diagnostic| diagnostic
                .message
                .to_lowercase()
                .contains(&expected.to_lowercase())),
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
fn unmatched_list_patterns_report_no_matching_case() {
    let error = common::run_value("fn main(){[42] match {[]=>0}}").unwrap_err();
    assert!(error.contains("NoMatchingCase"), "{error}");
}
