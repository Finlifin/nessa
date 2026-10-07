//! Intrinsic wrappers publish ordinary typed targets for frozen trait evidence.
mod common;

#[test]
fn integral_trait_equality_and_display_preserve_exact_types_and_values() {
    for ty in [
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    ] {
        let source = format!(
            "fn same(a:Eq,b:Eq)->bool{{a==b and not(a!=b)}};fn show(x:Display)->String{{x.to_string()}};fn main(){{let a:{ty}=42;let b:{ty}=41;if same(a,a) and not same(a,b) and show(a)==\"42\"{{42}}else{{0}}}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{source}");
    }
    let source = "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn show(x:Display)->String{x.to_string()};fn main(){let a:u128=18446744073709551616;let b:u128=18446744073709551617;if same(a,a) and not same(a,b) and show(b)==\"18446744073709551617\"{42}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bool_char_unit_type_and_string_trait_wrappers_have_real_semantics() {
    for source in [
        "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn show(x:Display)->String{x.to_string()};fn main(){if same(true,true) and not same(true,false) and show(true)==\"true\"{42}else{0}}",
        "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn show(x:Display)->String{x.to_string()};fn main(){if same('é','é') and not same('é','x') and type_of('é')==char and show('é')==\"é\"{42}else{0}}",
        "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn show(x:Display)->String{x.to_string()};fn main(){if same((),()) and show(())==\"()\"{42}else{0}}",
        "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn show(x:Display)->String{x.to_string()};fn main(){if same(i64,i64) and not same(i64,u64) and show(i64)==\"i64\"{42}else{0}}",
        "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn show(x:Display)->String{x.to_string()};fn main(){let a=\"é\"++\"x\";let b=\"éx\";if same(a,b) and not same(a,\"éy\") and show(a)==\"éx\"{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn floating_partial_equality_and_display_use_the_existing_numeric_contract() {
    for ty in ["f32", "f64"] {
        let source = format!(
            "fn same(a:PartialEq,b:PartialEq)->bool{{a==b and not(a!=b)}};fn show(x:Display)->String{{x.to_string()}};fn main(){{let a:{ty}=1.5;let b:{ty}=2.5;if same(a,a) and not same(a,b) and show(a)==\"1.5\"{{42}}else{{0}}}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{source}");
    }
    let source = "fn same(a:PartialEq,b:PartialEq)->bool{a.eq(b)};fn show(x:Display)->String{x.to_string()};fn main(){let nan:f64=sqrt(-1.0);let zero:f64=0.0;let negative:f64=-0.0;if not same(nan,nan) and same(zero,negative) and show(nan)==\"NaN\"{42}else{0}}";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn incompatible_self_types_and_unsupported_closure_values_cannot_forge_proofs() {
    for source in [
        "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn main(){let a:i64=42;let b:u64=42;if same(a,b){42}else{0}}",
        "fn show(x:Display)->String{x.to_string()};fn main(){let x:Any=||42;show(x);42}",
        "fn same(a:Eq,b:Eq)->bool{a.eq(b)};fn main(){let x:Any=||42;same(x,x);42}",
    ] {
        let result = common::run_value(source);
        assert!(
            matches!(result, Err(error) if error.contains("InvalidTraitProof") || error.contains("TypeError")),
            "{source}"
        );
    }
}

#[test]
fn ordinary_mixed_numeric_comparisons_keep_their_exact_existing_semantics() {
    for source in [
        "fn main(){let x:f32=0.5;let y:Any=x;let z:f32=y;if z==0.5{42}else{0}}",
        "fn main(){let x:i8=42;if x==42{42}else{0}}",
        "fn main(){let x:f32=0.1;let y:f64=0.1;if x!=y{42}else{0}}",
        "fn main(){let x:i128=-1;let y:u128=18446744073709551616;if x!=y{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

fn collect_at_print(context: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    assert!(context.collect_garbage()?);
    context.return_unit();
    Ok(())
}

fn collect_to_string(
    context: &mut interpreter::BuiltinCtx<'_>,
) -> Result<(), interpreter::VmError> {
    context.require_arity(1)?;
    let value = context.arg(0)?;
    assert!(context.collect_garbage()?);
    let text = context.format_value(value)?;
    context.return_string(&text)?;
    assert!(context.collect_garbage()?);
    Ok(())
}

#[test]
fn real_collection_preserves_string_proof_captures_and_detached_frames() {
    for source in [
        "fn capture(x:Display)->fn()->String{||x.to_string()};fn main(){let f=capture(\"abcd\"++\"efgh\");print(\"\");let text=f();if text==\"abcdefgh\"{42}else{0}}",
        "effect pause(catch k)->i64;fn work(x:Display)->String{pause()#;x.to_string()};fn main(){let saved=work(\"abcd\"++\"efgh\")# {pause(k)=>k};print(\"\");let first=saved(0);let second=saved(1);if first==\"abcdefgh\" and second==\"abcdefgh\"{42}else{0}}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(!compiled.has_errors, "{source}: {:?}", compiled.diagnostics);
        let mut engine = initialization::Engine::with_defaults();
        let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
            .unwrap()
            .unwrap();
        engine
            .vm_mut()
            .register_builtin(runtime::ids::PRINT, collect_at_print);
        engine
            .vm_mut()
            .register_builtin(runtime::ids::TO_STRING, collect_to_string);
        let before = engine.vm_mut().completed_collections();
        let task = engine.vm_mut().spawn_root(entry);
        assert!(
            matches!(engine.vm_mut().run(), interpreter::VmResult::Finished),
            "{source}"
        );
        assert!(engine.vm_mut().completed_collections() >= before + 3);
        assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
