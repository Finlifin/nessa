//! Known effect callers bind ordinary parameters independently of catch slots.

mod common;

use diagnostic::Level;

fn rejected(source: &str, message: &str) {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "accepted {source}");
    assert!(compiled.codegen_output.functions.is_empty(), "{source}");
    assert!(
        compiled.diagnostics.iter().any(|diagnostic| {
            diagnostic.level == Level::Error && diagnostic.message.contains(message)
        }),
        "{source}: {:?}",
        compiled.diagnostics
    );
    assert!(compiled.into_artifact().is_err(), "artifact for {source}");
}

#[test]
fn omitted_defaults_and_named_overrides_deliver_exact_effect_values() {
    for source in [
        "effect ask(.n:i64=42)->i64;fn main(){ask()#{ask(n)=>n}}",
        "effect ask(.n:i64=7)->i64;fn main(){ask(n=42)#{ask(n)=>n}}",
        "effect ask(a:i64,.b:i64=a+1,.c:i64=b+1)->i64;fn main(){ask(40)#{ask(a,b,c)=>c}}",
        "global calls:i64=0;fn fallback()->i64{calls+=1;7};effect ask(.n:i64=fallback())->i64;fn main(){let n=ask(n=42)#{ask(n)=>n};n+calls}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn explicit_named_values_evaluate_in_source_order_and_snapshot_before_later_writes() {
    for source in [
        "global order:i64=0;fn step(n:i64)->i64{order=order*10+n;n};effect pair(a:i64,b:i64)->i64;fn main(){pair(b=step(2),a=step(1))#{pair(a,b)=>if order==21 and a==1 and b==2{a*10+b+30}else{0}}}",
        "effect pair(a:i64,b:i64)->i64;fn main(){var x:i64=1;let value=pair(x,if true{x=2;x}else{0})#{pair(a,b)=>if a==1 and b==2{a*10+b}else{0}};if value==12 and x==2{value+30}else{0}}",
        "global order:i64=0;fn step(n:i64)->i64{order=order*10+n;n};effect ask(a:i64,.b:i64=step(2),.c:i64=step(3))->i64;fn main(){ask(a=step(1))#{ask(a,b,c)=>if order==123 and a==1 and b==2 and c==3{42}else{0}}}",
        "effect ask(a:i64,.b:i64=a+2)->i64;fn main(){var x:i64=40;let value=ask(a=x,b=if true{x=0;42}else{0})#{ask(a,b)=>if a==40 and b==42{b}else{0}};if x==0 and value==42{value}else{0}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn defaults_use_lexical_globals_and_complete_later_function_and_effect_headers() {
    for source in [
        "global base:i64=40;effect ask(.n:i64=base+2)->i64;fn main(){let base=0;ask()#{ask(n)=>n}}",
        "effect ask(.n:i64=later())->i64;fn later()->i64{42};fn main(){ask()#{ask(n)=>n}}",
        "effect ask(.n:i64=later()#{later()=>42})->i64;effect later()->i64;fn main(){ask()#{ask(n)=>n}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn catch_parameters_never_consume_caller_slots_even_after_varargs() {
    for source in [
        "effect ask(catch k,.n:i64=42)->i64;fn main(){ask()#{ask(k,n)=>k(n)}}",
        "effect ask(a:i64,catch k,.b:i64=a+2)->i64;fn main(){ask(40)#{ask(a,k,b)=>k(b)}}",
        "effect ask(a:i64,.b:i64=a+2,catch k)->i64;fn main(){ask(40)#{ask(a,b,k)=>k(b)}}",
        "effect pack(prefix:i64,...items:List,catch k,.extra:i64=items.len())->i64;fn main(){pack(1,20,19)#{pack(prefix,items,k,extra)=>k(prefix+items(0).as(i64)+items(1).as(i64)+extra)}}",
        "effect pack(catch k,.base:i64=40,...items:List,.extra:i64=2)->i64;fn main(){pack()#{pack(k,base,items,extra)=>if items.len()==0{k(base+extra)}else{0}}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn packed_varargs_preserve_heterogeneous_values_and_exceed_source_register_width() {
    let values = (0..80).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
    let source = format!(
        "effect pack(...items:List,catch k,.count:i64=items.len())->i64;fn main(){{pack({values})#{{pack(items,k,count)=>if count==80 and items(0)==0 and items(79)==79{{k(items(40).as(i64)+2)}}else{{0}}}}}}"
    );
    assert_eq!(common::run_value(&source), Ok(42));
    for source in [
        "typealias Values=List;effect pack(...items:Values,.count:i64=items.len())->i64;fn main(){pack(\"heap\",[42],null,())#{pack(items,count)=>if count==4 and items(0)==\"heap\" and items(2)==null and items(3)==(){items(1).as(List)(0).as(i64)}else{0}}}",
        "effect pack(...items:List,.answer:i64=40)->i64;fn main(){pack(2,answer=40)#{pack(items,answer)=>items(0).as(i64)+answer}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn narrow_optional_lambda_and_tuple_defaults_keep_their_types_and_previous_bindings() {
    for source in [
        "effect ask(.n:i8=42)->i64;fn main(){ask()#{ask(n)=>if type_of(n)==i8{n.as(i64)}else{0}}}",
        "effect ask(.n:?i64=null)->i64;fn main(){ask()#{ask(n)=>if n==null{42}else{0}}}",
        "effect ask(a:i64,.f:fn()->i64=||{return a+2})->i64;fn main(){ask(40)#{ask(a,f)=>f()}}",
        "effect ask((a,b):(i64,i64),.total:i64=a+b)->i64;fn main(){ask((40,2))#{ask(pair,total)=>total}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn defaults_keep_internal_loop_lambda_and_nested_handler_control_boundaries() {
    for source in [
        "effect ask(.n:i64=if true{var n:i64=0;while true{n+=1;if n<3{continue};break};n+39}else{0})->i64;fn main(){ask()#{ask(n)=>n}}",
        "effect inner()->i64;effect ask(.n:i64=inner()#{inner()=>{return 42}})->i64;fn main(){ask()#{ask(n)=>n}}",
        "effect ask(.n:i64=(||{return 42})())->i64;fn main(){ask()#{ask(n)=>n}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn unknown_duplicate_missing_and_wrong_effect_arguments_emit_no_artifact() {
    for (source, message) in [
        (
            "effect ask(a:i64)->i64;fn main(){ask(missing=42)#{ask(a)=>a}}",
            "unknown parameter",
        ),
        (
            "effect ask(a:i64)->i64;fn main(){ask(a=40,a=2)#{ask(a)=>a}}",
            "supplied more than once",
        ),
        (
            "effect ask(a:i64)->i64;fn main(){ask(40,a=2)#{ask(a)=>a}}",
            "supplied more than once",
        ),
        (
            "effect ask(a:i64)->i64;fn main(){ask()#{ask(a)=>a}}",
            "missing required",
        ),
        (
            "effect ask(.a:i64=42)->i64;fn main(){ask(true)#{ask(a)=>a}}",
            "too many positional",
        ),
        (
            "effect ask(a:i64)->i64;fn main(){ask(a=true)#{ask(a)=>a}}",
            "type mismatch",
        ),
        (
            "effect ask(...items:List)->i64;fn main(){ask(items=[])#{ask(items)=>42}}",
            "cannot be supplied by name",
        ),
        (
            "effect ask(catch k,.n:i64=42)->i64;fn main(){ask(k=42)#{ask(k,n)=>k(n)}}",
            "unknown parameter",
        ),
    ] {
        rejected(source, message);
    }
}

#[test]
fn unused_defaults_reject_bad_types_self_later_and_catch_references() {
    for (source, message) in [
        (
            "effect unused(.n:i64=true)->i64;fn main(){42}",
            "parameter default",
        ),
        (
            "effect unused(.n:i64=later())->i64;fn later()->bool{true};fn main(){42}",
            "parameter default",
        ),
        (
            "effect unused(.a:i64=a)->i64;fn main(){42}",
            "itself or a later parameter",
        ),
        (
            "effect unused(.a:i64=b,.b:i64=42)->i64;fn main(){42}",
            "itself or a later parameter",
        ),
        (
            "effect unused(catch k,.n:Continuation=k)->i64;fn main(){42}",
            "catch",
        ),
        (
            "effect unused(.n:Continuation=k,catch k)->i64;fn main(){42}",
            "catch",
        ),
        (
            "effect unused(.n:i8=128)->i64;fn main(){42}",
            "out of range",
        ),
    ] {
        rejected(source, message);
    }
}

#[test]
fn unused_invalid_variadic_declarations_and_default_controls_are_rejected() {
    for (source, message) in [
        (
            "effect unused(...a:List,...b:List)->i64;fn main(){42}",
            "dual variadic",
        ),
        (
            "effect unused(...a:i64)->i64;fn main(){42}",
            "requires a List",
        ),
        (
            "effect unused(...a:List,b:i64)->i64;fn main(){42}",
            "must follow all fixed",
        ),
        (
            "effect unused(.n:i64=if true{return 42}else{0})->i64;fn main(){42}",
            "default boundary",
        ),
        (
            "effect unused(.n:i64=if true{resume 42}else{0})->i64;fn main(){42}",
            "default boundary",
        ),
        (
            "effect unused(.n:i64=if true{break;42}else{0})->i64;fn main(){42}",
            "require a loop inside",
        ),
        (
            "effect unused(.n:i64=if true{continue;42}else{0})->i64;fn main(){42}",
            "require a loop inside",
        ),
    ] {
        rejected(source, message);
    }
}

#[test]
fn selected_effect_defaults_enter_recursion_and_lexical_initialization_plans() {
    for source in [
        "effect ask(.n:i64=ask(n=42)#{ask(n)=>n})->i64;fn main(){ask()#{ask(n)=>n}}",
        "effect ask(.n:i64=storage.n)->i64;mod api{pub global answer:i64=ask()#{ask(n)=>n}};mod storage{pub global n:i64=42};fn main(){api.answer}",
        "effect ask(.n:i64=api.answer)->i64;mod api{pub global answer:i64=ask(n=42)#{ask(n)=>n}};fn main(){api.answer}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
    rejected(
        "effect ask(.n:i64=ask()#{ask(n)=>n})->i64;fn main(){42}",
        "recursive default argument expansion",
    );
    rejected(
        "effect ask(.n:i64=storage.n)->i64;mod api{pub global answer:i64=ask()#{ask(n)=>n}};mod storage{pub global n:i64=api.answer};fn main(){api.answer}",
        "cyclic module value initialization dependency",
    );
}

#[test]
fn erased_default_values_still_fail_runtime_parameter_checks() {
    for source in [
        "fn wrong()->Any{true};effect ask(.n:i64=wrong())->i64;fn main(){ask()#{ask(n)=>42};42}",
        "fn wrong()->Any{true};effect ask(a:i64,.b:i64=a)->i64;fn main(){ask(wrong())#{ask(a,b)=>42};42}",
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}

#[test]
fn effect_defaults_check_inferred_function_returns_in_both_declaration_orders() {
    for declarations in [
        "effect ask(.x:i64=later())->i64;fn later(){true};",
        "fn later(){true};effect ask(.x:i64=later())->i64;",
        "effect ask(.x:i64=wrapper())->i64;fn wrapper(){leaf()};fn leaf()->bool{true};",
        "fn wrapper(){leaf()};fn leaf()->bool{true};effect ask(.x:i64=wrapper())->i64;",
    ] {
        for main in ["fn main(){42}", "fn main(){ask()#{ask(x)=>x}}"] {
            rejected(&format!("{declarations}{main}"), "type mismatch");
        }
    }
    for source in [
        "effect ask(.x:i64=later())->i64;fn later(){42};fn main(){ask()#{ask(x)=>x}}",
        "fn later(){42};effect ask(.x:i64=later())->i64;fn main(){ask()#{ask(x)=>x}}",
        "effect ask(.x:i64=wrapper())->i64;fn wrapper(){leaf()};fn leaf()->i64{42};fn main(){ask()#{ask(x)=>x}}",
        "fn wrapper(){leaf()};fn leaf()->i64{42};effect ask(.x:i64=wrapper())->i64;fn main(){ask()#{ask(x)=>x}}",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}
