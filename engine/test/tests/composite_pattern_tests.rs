//! Composite patterns execute across matching, guards, captures, and iteration.

mod common;

#[test]
fn alternatives_match_scalar_kinds_and_nested_payloads() {
    for source in [
        "fn main(){3 match {1 or 2 or 3=>42,_=>0}}",
        "fn main(){\"right\" match {\"left\" or \"right\"=>42,_=>0}}",
        "fn main(){'界' match {'a' or '界'=>42,_=>0}}",
        "fn main(){let x:Any=null;x match {() or null=>42,_=>0}}",
        "fn main(){let x:Any=();x match {null or ()=>42,_=>0}}",
        "enum E{a,b,c}\nfn main(){E.b match {E.a or E.b=>42,_=>0}}",
        "enum E{pair(value:(i64,char))}\nfn main(){E.pair((2,'界')) match {E.pair((1 or 2,'a' or '界'))=>42,_=>0}}",
        "enum E{a(value:i64),b(value:i64),c(value:i64)}\nfn main(){E.c(42) match {(E.a(n) or E.b(n)) or E.c(n)=>n}}",
        "fn main(){(42,) match {(x,)=>x}}",
        "fn main(){42 match {(((42)))=>42,_=>0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn failed_partial_alternatives_replace_all_shared_bindings() {
    for source in [
        "fn main(){(10,42) match {(x,0) or (_,x)=>x,_=>0}}",
        "fn main(){(40,2) match {((x,y) if false) or (y,x)=>x-y+80,_=>0}}",
        "enum E{pair(value:(i64,i64))}\nfn main(){E.pair((10,42)) match {E.pair((x,0) or (_,x))=>x,_=>0}}",
        "fn main(){let f=(10,42) match {(x,0) or (_,x)=>||x,_=>||0};f()}",
        "fn main(){let f=(42,0) match {(x,0) or (_,x)=>||x,_=>||0};f()}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn aliases_preserve_whole_values_and_precise_payload_types() {
    for source in [
        "typealias Pair=(i64,i64)\nenum E{pair(value:Pair)}\nfn main(){E.pair((40,2)) match {E.pair((x,y) as pair) as whole=>if pair'type==Pair and whole'type==E {x+y+pair.1-2}else{0}}}",
        "fn main(){let n:i8=42;n match {x as whole=>if x'type==i8 and whole'type==i8 {whole}else{0}}}",
        "fn main(){let value:Any=(40,2);value match {_ as whole=>{let pair:(i64,i64)=whole;pair.0+pair.1}}}",
        "enum E{some(value:i64),none}\nfn main(){let value:Any=E.some(42);value match {(E.none or E.some(_)) as whole=>whole match {E.some(n)=>n,_=>0},_=>0}}",
        "fn main(){42 match {42 as whole if whole==42=>whole,_=>0}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn guards_short_circuit_and_outer_failure_does_not_retry() {
    for (source, expected) in [
        (
            "global trace:i64=0\nfn mark(n:i64,answer:bool)->bool{trace=trace*10+n;answer}\nfn main(){let value=42 match {(x if mark(1,true)) or (x if mark(2,true))=>x,_=>0};trace*100+value}",
            142,
        ),
        (
            "global trace:i64=0\nfn mark(n:i64,answer:bool)->bool{trace=trace*10+n;answer}\nfn main(){let value=42 match {(x if mark(1,false)) or (x if mark(2,x==42))=>x,_=>0};trace*100+value}",
            1242,
        ),
        (
            "global trace:i64=0\nfn mark(n:i64,answer:bool)->bool{trace=trace*10+n;answer}\nfn main(){let value=42 match {(x if mark(1,true)) or (x if mark(2,true)) if mark(3,false)=>0,_=>42};trace*100+value}",
            1342,
        ),
        (
            "global count:i64=0\nfn input()->i64{count+=1;42}\nfn main(){let value=input() match {0 or 42=>42,_=>0};count*100+value}",
            142,
        ),
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            expected,
            "{source}"
        );
    }
}

#[test]
fn matches_guards_can_read_the_successful_alternative_and_alias() {
    for source in [
        "fn main(){if ((10,42) matches ((x,0) or (_,x)) as whole if (x==42 and whole.0==10)) {42}else{0}}",
        "enum E{a(value:i64),b(value:i64)}\nfn main(){if (E.b(42) matches (E.a(n) or E.b(n)) as whole if (n==42 and whole'type==E)) {42}else{0}}",
        "fn main(){if (42 matches (0 or 42) if false) {0}else{42}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn refutable_for_patterns_skip_failures_and_capture_both_alternatives() {
    for source in [
        "enum E{a(value:i64),b(value:i64),none}\nfn main(){let total:i64=0;for (E.a(n) or E.b(n)) as whole if n>0 and whole'type==E in [null,E.a(20),E.none,E.b(22),E.a(-1),()]{let f=||n;total+=f()};total}",
        "fn main(){let total:i64=0;for (null or ()) as value in [null,42,(),\"skip\"]{if value==null{total+=20}else{total+=22}};total}",
        "global count:i64=0\nfn values()->List{count+=1;[1,2,3]}\nfn main(){let total:i64=0;for (1 or 2) as n in values(){total+=n.as(i64)};count*39+total}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn transparent_aliases_and_identical_narrow_binding_types_are_accepted() {
    for source in [
        "typealias Tiny=i8\nenum E{a(value:Tiny),b(value:i8)}\nfn main(){E.b(42) match {E.a(n) or E.b(n)=>if n'type==i8 {n}else{0}}}",
        "typealias Tiny=i8\nenum E{a(value:Tiny),b(value:i8)}\nfn main(){E.a(42) match {E.a(n) or E.b(n)=>if n'type==Tiny {n}else{0}}}",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn unequal_binding_types_and_escaped_matches_bindings_are_rejected() {
    for (source, expected) in [
        (
            "enum E{a(value:i8),b(value:i64)}\nfn main(){E.a(42) match {E.a(n) or E.b(n)=>n}}",
            "same type",
        ),
        (
            "enum E{a(value:Any),b(value:i64)}\nfn main(){E.a(42) match {E.a(n) or E.b(n)=>n}}",
            "same type",
        ),
        (
            "enum E{a(value:i64)}\nfn main(){E.a(42) match {E.a(n) or (_ as n)=>42}}",
            "same type",
        ),
        ("fn main(){42 matches (x or x) if x==42;x}", "undefined"),
        ("fn main(){for 0 or x in [42]{x};42}", "same names"),
        (
            "fn main(){42 match {(x as whole) or (x as other)=>42}}",
            "same names",
        ),
        (
            "fn main(){(40,2) match {((x as y),y)=>42}}",
            "same name more than once",
        ),
        ("fn main(){42 match {42 as 42=>42}}", "identifier binding"),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(compiled.codegen_output.functions.is_empty(), "{source}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|error| error.message.contains(expected)),
            "{source}: {:?}",
            compiled.diagnostics
        );
        assert!(
            compiled.into_artifact().is_err(),
            "created artifact for {source}"
        );
    }
}
